"""Offline contracts for the canonical Telegram report; never contact Telegram."""
import importlib
import io
import os
import unittest
from unittest.mock import patch

from scripts import pirana_report as report


def fixture():
    return dict(accounting=report.canonical_unavailable('accounting incomplete'),
                snapshot_source='fixture/accounting.json', report_generated_at_ms=1789646400000)


def test_unknown_balances_and_history_are_explicit():
    text = report.format_text_report(fixture())
    assert text.startswith('₿ BTC podle bota: NEOVĚŘENO sats')
    assert 'USD podle bota: NEOVĚŘENO USD' in text
    assert 'Celá pokrytá historie: NEOVĚŘENO' in text
    assert 'Active samo nepotvrzuje' in text
    assert 'KONTROLA:' in text
    assert 'PŮVOD DAT' in text
    assert 'sync do: NEOVĚŘENO' in text


def test_zero_new_period_does_not_claim_zero_fills_or_known_history():
    data = fixture()
    data['accounting']['active_period'] = dict(start_ms=1789646400000,
        status='complete', closed_count=0, gross_pnl_usd='0', net_pnl_usd='0', fees_usd='0')
    text = report.format_text_report(data)
    assert 'Celá pokrytá historie: NEOVĚŘENO' in text
    assert 'Nové období od ' in text
    assert 'čistý PnL 0 USD' in text
    assert 'prodejní plnění 0' in text
    assert 'nevylučuje BUY filly' in text


def test_html_escape_and_diagnostics_bound():
    data = fixture()
    data['accounting']['issues'] = ['<unsafe>&' * 1000] * 100
    data['snapshot_source'] = '<fixture>'
    text = report.format_telegram_html(data)
    assert '<unsafe>' not in text
    assert '&lt;fixture&gt;' in text
    # Telegram counts text after entity parsing.
    import html
    assert len(html.unescape(text)) < 4096


def test_scheduled_failure_reaches_exit_code_without_network():
    with patch.dict(os.environ, {'TELEGRAM_BOT_TOKEN': 'fixture', 'TELEGRAM_CHAT_ID': '123'}):
        module = importlib.import_module('scripts.send_scheduled_report')
    for success, expected in [(True, 0), (False, 1)]:
        with patch.object(module, 'build_report', return_value='fixture'), patch.object(
                module, 'send_telegram', return_value=success), patch('builtins.open', return_value=io.StringIO()):
            assert module.main() == expected


def test_runtime_balances_are_labeled_and_never_added_to_pnl():
    data = fixture()
    data['runtime'] = dict(btc_balance=0.00051, locked_btc_reserve=0.0001,
                           usd_balance=123.45, system_mode='Active', daily_pnl=9999)
    text = report.format_text_report(data)
    assert '51000 sats' in text and '10000 sats' in text and '123.45 USD' in text
    assert '9999' not in text
    assert 'čas ověření peněženky burzou není doložen' in text
    for invalid in (None, True, -1, 'NaN', float('inf'), {}):
        data['runtime']['btc_balance'] = invalid
        assert 'BTC podle bota: NEOVĚŘENO sats' in report.format_text_report(data)


def test_no_api_runtime_does_not_contact_network(tmp_path):
    with patch.object(report.urllib.request, 'urlopen', side_effect=AssertionError('network')):
        data = report.generate_report_data(snapshot_file=str(tmp_path/'missing'),
            ledger_path=str(tmp_path/'absent'), include_runtime=True, no_api=True)
    assert data['runtime'] is None


def test_optional_runtime_capture_keeps_accounting_separate(tmp_path):
    with patch.object(report, 'fetch_snapshot', return_value=(
            {'btc_balance': 0.00051, 'daily_pnl': 9999}, 'LOCAL_API: fixture', [])) as fetch:
        data = report.generate_report_data(snapshot_file=str(tmp_path/'missing'),
            ledger_path=str(tmp_path/'absent'), include_runtime=True, no_api=False)
    fetch.assert_called_once_with(snapshot_file=None, no_api=False)
    assert data['accounting']['daily']['net_pnl_usd'] is None
    assert '51000 sats' in report.format_text_report(data)
    assert '9999' not in report.format_text_report(data)


def test_partial_sell_is_execution_not_closed_position(tmp_path):
    import datetime as dt
    from scripts import pirana_accounting as accounting
    now_ms = 1789682400000
    con = accounting.connect(tmp_path / 'partial.sqlite3', True)
    try:
        def fill(tid, amount):
            return dict(trade_id=tid, order_id=tid+100, symbol='tBTCUSD',
                mts=now_ms-100+tid, exec_amount=amount, exec_price='100',
                fee='0', fee_currency='USD', cid=None)
        accounting.ingest(con, dict(fills=[fill(1, '1'), fill(2, '-0.25')],
            sync=dict(start_ms=0, end_ms=now_ms, complete=True)))
        snapshot = accounting.snapshot(con, dt.datetime.fromtimestamp(now_ms/1000, dt.timezone.utc))
    finally:
        con.close()
    assert snapshot['lifetime']['closed_count'] == 1
    assert snapshot['open_lots'][0]['remaining_btc'] == '0.75'
    text = report.format_text_report(dict(accounting=snapshot, snapshot_source='fixture'))
    assert 'prodejní plnění 1' in text
    assert 'Uzavřené pozice' not in text


def test_incomplete_metadata_preserved_but_never_authorizes_pnl(tmp_path):
    import json
    now_ms = 1789682400000
    raw = dict(schema_version=1, source='authenticated_bitfinex_fills', scope='account:tBTCUSD',
        status='incomplete', generated_at_ms=now_ms,
        sync=dict(complete=True, cursor_ms=now_ms),
        issues=['unmatched_sell_cost:1', 'unmatched_sell_cost:2', 'unmatched_sell_cost:<bad>'],
        daily=dict(net_pnl_usd='999'), lifetime=dict(net_pnl_usd='999'))
    path = tmp_path / 'snapshot.json'
    path.write_text(json.dumps(raw))
    data = report.generate_report_data(snapshot_file=str(path), ledger_path=str(tmp_path/'absent'),
                                      no_api=True, now_arg=str(now_ms/1000))
    assert data['source_metadata'] == dict(generated_at_ms=now_ms, cursor_ms=now_ms)
    assert data['accounting']['daily']['net_pnl_usd'] is None
    text = report.format_text_report(data)
    assert 'U 2 prodejních plnění chybí pořizovací cena' in text
    assert '999' not in text and '<bad>' not in text
    assert 'sync do: NEOVĚŘENO' not in text


def test_cli_enables_runtime_and_no_api_remains_offline(tmp_path):
    with patch('sys.argv', ['pirana_report.py', '--no-api', '--snapshot-file', str(tmp_path/'absent'),
                            '--ledger', str(tmp_path/'absent')]), patch.object(
            report.urllib.request, 'urlopen', side_effect=AssertionError('network')), patch.object(
            report, 'fetch_snapshot', wraps=report.fetch_snapshot) as fetch:
        assert report.main() == 0
    fetch.assert_called_once_with(snapshot_file=None, no_api=True)
    with patch('sys.argv', ['pirana_report.py']), patch.object(report, 'generate_report_data',
            return_value=fixture()) as generate:
        assert report.main() == 0
    assert generate.call_args.kwargs['include_runtime'] is True


def test_monthly_main_has_no_exchange_query_or_invented_metrics():
    module = importlib.import_module('scripts.send_monthly_report')
    with patch.object(module, 'generate_report_data', return_value=fixture()), patch.object(
            module, 'load_env', return_value={}), patch.object(module, 'fetch_bitfinex_trades',
            side_effect=AssertionError('exchange')), patch.object(module, 'get_snapshot',
            side_effect=AssertionError('old runtime path')), patch('sys.argv', ['monthly', '--dry-run']), patch(
            'sys.stdout', new_callable=io.StringIO) as output:
        assert module.main() == 0
    text = output.getvalue()
    assert 'Měsíční PnL' in text and 'NEOVĚŘENO' in text
    assert '99.98' not in text and '393.56' not in text


def test_daily_timeout_fallback_uses_canonical_report_and_single_delivery(tmp_path):
    import subprocess
    from pathlib import Path
    script = Path('scripts/daily_check.sh').read_text()
    stub_dir = tmp_path/'bin'
    stub_dir.mkdir()
    (stub_dir/'timeout').write_text('#!/bin/sh\nexit 124\n')
    (stub_dir/'python3').write_text('''#!/bin/sh
if [ "$1" = "-c" ]; then
  cat > "$CAPTURE_REPORT"
  echo delivery >> "$CAPTURE_SENDS"
  exit "${DELIVERY_EXIT:-0}"
fi
printf 'CANONICAL FINANCIAL FIXTURE\\n'
''')
    for path in stub_dir.iterdir():
        path.chmod(0o755)
    script = script.replace('/home/wwwenda/workspace/pirana', str(tmp_path))
    script = script.replace('export PATH="', 'export PATH="'+str(stub_dir)+':', 1)
    path = tmp_path/'daily.sh'
    path.write_text(script)
    env = dict(os.environ, TELEGRAM_TOKEN='fixture', CHAT_ID='123',
               CAPTURE_REPORT=str(tmp_path/'report'), CAPTURE_SENDS=str(tmp_path/'sends'))
    result = subprocess.run(['bash', str(path)], env=env, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    text = (tmp_path/'report').read_text()
    assert 'CANONICAL FINANCIAL FIXTURE' in text and 'TIMEOUT' in text
    assert 'NEOVĚŘENÉ KVALITATIVNÍ HODNOCENÍ' in text
    assert (tmp_path/'sends').read_text() == 'delivery\n'
    assert '100% stabilní a ziskový' not in script
    env['DELIVERY_EXIT'] = '1'
    assert subprocess.run(['bash', str(path)], env=env, capture_output=True).returncode == 1


# Preserve the production /status handler regression coverage.
"""Exercise only status handler AST; importing the bot loads credentials."""
import ast
import asyncio
import html
import sys
import types
from pathlib import Path

BOT = Path(os.environ.get(
    "CASLAV_TELEGRAM_BOT_PATH",
    "/home/wwwenda/workspace/caslav_telegram/caslav_bot.py",
))
EXTERNAL_BOT_AVAILABLE = BOT.is_file()


def run_handler(rc, text):
    tree = ast.parse(BOT.read_text())
    node = next(n for n in tree.body if isinstance(n, ast.AsyncFunctionDef) and n.name == 'cmd_status')
    messages = []
    async def send(chat, body):
        messages.append(body)
    async def to_thread(fn, *args):
        return fn(*args)
    ns = dict(asyncio=types.SimpleNamespace(to_thread=to_thread), html=html,
              sys=sys, send=send, run_cmd=lambda *args: (rc, text))
    exec(compile(ast.Module(body=[node], type_ignores=[]), str(BOT), 'exec'), ns)
    asyncio.run(ns['cmd_status'](0))
    return messages


@unittest.skipUnless(EXTERNAL_BOT_AVAILABLE, "external caslav_telegram bot is not part of this repository")
def test_status_escapes_and_preserves_chunks():
    text = '<unsafe> & PnL\n' * 500
    messages = run_handler(0, text)
    assert len(messages) > 1
    assert ''.join(html.unescape(m[5:-6]) for m in messages) == text
    assert all('<unsafe>' not in m for m in messages)


@unittest.skipUnless(EXTERNAL_BOT_AVAILABLE, "external caslav_telegram bot is not part of this repository")
def test_failure_does_not_publish_subprocess_stderr():
    messages = run_handler(1, 'private traceback')
    assert len(messages) == 1
    assert 'private' not in messages[0]


def test_execution_block_is_reported_and_html_escaped():
    data = fixture()
    data['runtime'] = dict(system_mode='Halted', execution_block_reason='<unconfirmed order>')
    assert 'Blokace obchodování: <unconfirmed order>' in report.format_text_report(data)
    assert '&lt;unconfirmed order&gt;' in report.format_telegram_html(data)
