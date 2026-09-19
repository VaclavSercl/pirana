import importlib.util
import subprocess
from pathlib import Path


def _strategy(position):
    return f"""
[system]
reload_interval_seconds = 60
[trading]
trade_size_btc = 0.0002
max_open_orders = 3
[strategy]
entry_zone_spread_usd = 5.0
take_profit_distance_usd = 15.0
stop_loss_distance_usd = 400.0
ofi_trigger_threshold = 0.90
ofi_window_size = 100
trade_cooldown_ms = 2000
min_confidence_score = 0.95
[inventory]
min_inventory_btc = 0.0001
max_inventory_btc = 0.05
target_inventory_pct = 30.0
target_inventory_btc = 0.01
use_dynamic_inventory = true
[risk_management]
max_slippage_bps = 5
position_size_pct = {position}
max_aggregate_exposure_pct = 90.0
max_single_trade_risk_pct = 5.0
use_dynamic_winrate_sizing = true
min_position_size_pct = 1.0
max_position_size_pct = 25.0
[volatility]
use_dynamic_atr = true
atr_period = 14
ticks_per_bar = 6
atr_tp_multiplier = 0.5
atr_sl_multiplier = 0.5
min_tp_usd = 10.0
max_tp_usd = 120.0
min_sl_usd = 30.0
max_sl_usd = 150.0
[order_book]
use_l2_depth_imbalance = true
l2_depth_levels = 5
l2_weight_decay = 0.5
l2_weight_alpha = 0.40
min_l2_imbalance_threshold = 0.15
[trailing_stop]
enabled = true
min_trigger_usd = 6.0
be_offset_usd = 2.0
trail_multiplier = 0.3
[profit_skimmer]
enabled = true
btc_lock_pct = 10.0
exclude_from_trading_margin = true
[adaptive_cooldown]
enabled = true
min_ms = 1000
max_ms = 20000
[lead_lag]
enabled = true
[hawkes_process]
enabled = true
[vpin_guard]
enabled = true
[avellaneda_stoikov]
enabled = true
"""


def _load_versioner():
    path = Path(__file__).parents[1] / "scripts" / "strategy_versioning.py"
    spec = importlib.util.spec_from_file_location("strategy_versioning_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


def _repo(tmp_path):
    repo = tmp_path / "repo"
    remote = tmp_path / "remote.git"
    repo.mkdir()
    _git(repo, "init", "-b", "fix/test")
    _git(repo, "config", "user.name", "test")
    _git(repo, "config", "user.email", "test@example.invalid")
    (repo / "strategy.toml").write_text(_strategy("1.0"))
    _git(repo, "add", "strategy.toml")
    _git(repo, "commit", "-m", "base")
    subprocess.run(["git", "init", "--bare", remote], check=True, capture_output=True)
    _git(repo, "remote", "add", "origin", str(remote))
    _git(repo, "push", "-u", "origin", "fix/test")
    return repo, remote


def test_strategy_versioning_pushes_current_branch_and_backs_up_previous(tmp_path, monkeypatch):
    repo, remote = _repo(tmp_path)
    module = _load_versioner()
    monkeypatch.setattr(module, "REPO_DIR", str(repo))
    monkeypatch.setattr(module, "STRATEGY_FILE", str(repo / "strategy.toml"))
    monkeypatch.setattr(module, "BACKUP_FILE", str(repo / "strategy.toml.bak"))
    monkeypatch.setattr(module, "REMOTE", "origin")

    (repo / "strategy.toml").write_text(_strategy("2.0"))
    assert module.commit_strategy("test branch-safe publish")
    assert "position_size_pct = 1.0" in (repo / "strategy.toml.bak").read_text()
    assert _git(repo, "branch", "--show-current").stdout.strip() == "fix/test"

    remote_text = subprocess.run(
        ["git", f"--git-dir={remote}", "show", "refs/heads/fix/test:strategy.toml"],
        check=True, capture_output=True, text=True,
    ).stdout
    assert "position_size_pct = 2.0" in remote_text


def test_detached_head_refuses_publish_and_restores_committed_strategy(tmp_path, monkeypatch):
    repo, _ = _repo(tmp_path)
    module = _load_versioner()
    monkeypatch.setattr(module, "REPO_DIR", str(repo))
    monkeypatch.setattr(module, "STRATEGY_FILE", str(repo / "strategy.toml"))
    monkeypatch.setattr(module, "BACKUP_FILE", str(repo / "strategy.toml.bak"))

    _git(repo, "checkout", "--detach")
    (repo / "strategy.toml").write_text(_strategy("2.0"))
    assert not module.commit_strategy("must fail detached")
    assert "position_size_pct = 1.0" in (repo / "strategy.toml").read_text()


def test_invalid_hard_cap_is_rejected(tmp_path, monkeypatch):
    repo, _ = _repo(tmp_path)
    module = _load_versioner()
    monkeypatch.setattr(module, "REPO_DIR", str(repo))
    monkeypatch.setattr(module, "STRATEGY_FILE", str(repo / "strategy.toml"))
    monkeypatch.setattr(module, "BACKUP_FILE", str(repo / "strategy.toml.bak"))

    bad = _strategy("2.0").replace(
        "max_aggregate_exposure_pct = 90.0",
        "max_aggregate_exposure_pct = 95.0",
    )
    (repo / "strategy.toml").write_text(bad)
    assert not module.commit_strategy("must reject unsafe hard cap")
    assert "max_aggregate_exposure_pct = 90.0" in (repo / "strategy.toml").read_text()


def test_safe_config_guard_never_rolls_back_or_restarts_automatically():
    from pathlib import Path
    script = Path("scripts/safe_config_update.sh").read_text()
    auto = script.split("auto-guard)", 1)[1].split(";;", 1)[0]
    assert "rollback" not in auto
    assert "systemctl restart" not in auto
    assert 'SNAPSHOT_URL="http://127.0.0.1:8080/api/snapshot"' in script
    manual = script.split("rollback)", 1)[1].split(";;", 1)[0]
    assert "strategy_versioning.py" not in manual or "VERSIONER" in manual
    assert "systemctl restart" not in manual
