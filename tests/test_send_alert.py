"""Offline regressions; every HTTP request is mocked."""
import io
import json
from unittest.mock import Mock
import pytest
from scripts import send_alert as alert


@pytest.fixture(autouse=True)
def isolated(monkeypatch):
    monkeypatch.delenv('TELEGRAM_BOT_TOKEN', raising=False)
    monkeypatch.delenv('TELEGRAM_CHAT_ID', raising=False)
    monkeypatch.setattr(alert, 'get_journal_snippet', lambda _: '<&>' * 1000)
    monkeypatch.setattr('sys.argv', ['send_alert', 'fixture.service'])
    monkeypatch.setattr(alert.urllib.request, 'urlopen', Mock(side_effect=AssertionError('unmocked network')))


def transport(monkeypatch, payload=b'{"ok":true}', status=200):
    response = io.BytesIO(payload)
    response.status = status
    call = Mock(return_value=response)
    monkeypatch.setattr(alert.urllib.request, 'urlopen', call)
    return call


def test_systemd_env_does_not_read_inaccessible_dotenv(monkeypatch):
    monkeypatch.setenv('TELEGRAM_BOT_TOKEN', 'fixture-token')
    monkeypatch.setenv('TELEGRAM_CHAT_ID', 'fixture-chat')
    fallback = Mock(side_effect=PermissionError('denied'))
    monkeypatch.setattr(alert, 'load_env', fallback)
    call = transport(monkeypatch)
    assert alert.send_alert() == 0
    fallback.assert_not_called()
    call.assert_called_once()
    body = alert.urllib.parse.parse_qs(call.call_args.args[0].data.decode())
    assert body['chat_id'] == ['fixture-chat']
    assert '&lt;&amp;&gt;' in body['text'][0]


def test_partial_env_preserved_with_fallback(monkeypatch):
    monkeypatch.setenv('TELEGRAM_BOT_TOKEN', 'injected-token')
    monkeypatch.setattr(alert, 'load_env', lambda: {'TELEGRAM_BOT_TOKEN':'other','TELEGRAM_CHAT_ID':'fallback-chat'})
    call = transport(monkeypatch)
    assert alert.send_alert() == 0
    assert '/botinjected-token/' in call.call_args.args[0].full_url


def test_missing_env_inaccessible_fallback_fails_safely(monkeypatch, capsys):
    monkeypatch.setattr(alert, 'load_env', Mock(side_effect=PermissionError('secret-detail')))
    assert alert.send_alert() == 2
    assert 'secret-detail' not in capsys.readouterr().err
    alert.urllib.request.urlopen.assert_not_called()


@pytest.mark.parametrize('payload,status', [(b'{"ok":false}',200),(b'{}',200),(b'invalid',200),(b'{"ok":true}',500),(b'x'*65537,200)])
def test_http_alone_does_not_claim_delivery(monkeypatch,payload,status):
    monkeypatch.setattr(alert, 'load_env', lambda: {'TELEGRAM_BOT_TOKEN':'fixture','TELEGRAM_CHAT_ID':'fixture'})
    call=transport(monkeypatch,payload,status)
    assert alert.send_alert()==1
    call.assert_called_once()


def test_uncertain_post_never_retried_or_secret_logged(monkeypatch,capsys):
    monkeypatch.setattr(alert, 'load_env', lambda: {'TELEGRAM_BOT_TOKEN':'fixture','TELEGRAM_CHAT_ID':'fixture'})
    call=Mock(side_effect=TimeoutError('https://api.telegram.org/botSECRET/sendMessage'))
    monkeypatch.setattr(alert.urllib.request,'urlopen',call)
    assert alert.send_alert()==1
    call.assert_called_once()
    assert 'SECRET' not in capsys.readouterr().err
