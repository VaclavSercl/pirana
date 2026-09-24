//! Process-level regression: no scenario may invoke a trading mutation.
#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    process::Command,
};

#[test]
fn diagnostics_never_restart_on_missing_recovery_proof() {
    let base = std::env::temp_dir().join(format!("doctor-regression-{}", std::process::id()));
    fs::create_dir(&base).unwrap();
    let script = base.join("fake-command");
    fs::write(
        &script,
        r##"#!/bin/sh
printf '%s %s\n' "${0##*/}" "$*" >> "$CALL_LOG"
case "${0##*/}" in
 systemctl) exit "$SERVICE_RC";;
 curl) printf '%s' "$SNAPSHOT"; exit "$CURL_RC";;
 journalctl) printf '%s' "$JOURNAL"; exit "$JOURNAL_RC";;
 timeout) exit 0;;
 *) exit 99;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    for cmd in ["systemctl", "curl", "journalctl", "timeout", "sudo"] {
        symlink(&script, base.join(cmd)).unwrap();
    }
    let healthy = serde_json::json!({"system_mode":"Active", "market_data_available":true,
        "btc_price":60000.0, "uptime_seconds":99000, "execution_block_reason":null});
    for case in 0..10 {
        let mut snap = healthy.clone();
        match case {
            3 => snap["market_data_available"] = false.into(),
            4 => snap["system_mode"] = "Halted".into(),
            5 => snap["system_mode"] = "Defensive".into(),
            6 => snap["execution_block_reason"] = "pending order".into(),
            9 => {
                snap.as_object_mut()
                    .unwrap()
                    .remove("market_data_available");
            }
            _ => (),
        }
        let log = base.join(format!("calls-{case}"));
        let output = Command::new(env!("CARGO_BIN_EXE_caslav-doctor"))
            .arg("check").env("PATH", &base).env("CALL_LOG", &log)
            .env("SERVICE_RC", if case == 1 {"3"} else {"0"})
            .env("CURL_RC", if case == 2 {"22"} else {"0"})
            .env("JOURNAL_RC", if case == 7 {"1"} else {"0"})
            .env("SNAPSHOT", snap.to_string())
            .env("JOURNAL", if case == 8 {"panicked at old process"} else {
                "tungstenite WebSocket closed Connection reset\nWebSocket closed\nWebSocket closed"
            }).output().unwrap();
        let calls = fs::read_to_string(log).unwrap();
        assert!(
            !calls.lines().any(|line| line.starts_with("sudo ")),
            "{case}: {calls}"
        );
        assert!(calls
            .lines()
            .filter(|s| s.starts_with("systemctl"))
            .all(|s| s == "systemctl is-active pirana.service"));
        let text = String::from_utf8(output.stdout).unwrap();
        if case == 0 {
            assert!(output.status.success(), "{text}");
            assert!(text.contains("obchodní aktivita NEOVĚŘENO"));
            assert!(text.contains("historické výpadky nejsou důvodem k restartu"));
            assert!(!calls.contains("timeout"));
        } else {
            assert_eq!(output.status.code(), Some(2), "{case}: {text}");
            assert!(text.contains("chybí aktuální ověření příkazů"));
            assert!(calls.contains("timeout"));
        }
    }
    fs::remove_dir_all(base).unwrap();
}
