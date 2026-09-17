"""Exercise only status handler AST; importing the bot loads credentials."""
import ast
import asyncio
import html
import sys
import types
from pathlib import Path

BOT = Path('/home/wwwenda/workspace/caslav_telegram/caslav_bot.py')


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


def test_status_escapes_and_preserves_chunks():
    text = '<unsafe> & PnL\n' * 500
    messages = run_handler(0, text)
    assert len(messages) > 1
    assert ''.join(html.unescape(m[5:-6]) for m in messages) == text
    assert all('<unsafe>' not in m for m in messages)


def test_failure_does_not_publish_subprocess_stderr():
    messages = run_handler(1, 'private traceback')
    assert len(messages) == 1
    assert 'private' not in messages[0]
