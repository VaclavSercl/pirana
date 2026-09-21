#!/usr/bin/env python3
"""Idempotent shell installer for SynthBit."""
from __future__ import annotations
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

from common import Blocked, repo_root, safe

BLOCK_START = '# >>> SynthBit managed'
BLOCK_END = '# <<< SynthBit managed'

BASHRC_SNIPPET = f"""
{BLOCK_START}
# SynthBit managed aliases
export PATH="$HOME/.local/bin:$PATH"
alias ai='ai-run'
alias ai-claude='ai-run claude'
alias ai-codex='ai-run codex'
alias ai-gemini='ai-run gemini'
alias ai-hermes='ai-run hermes'
alias ai-undo='ai-run undo'
# <<< SynthBit managed
"""


def find_bashrc(home: Path) -> Path | None:
    """Find or determine bashrc path."""
    candidates = [
        home / '.bashrc',
        home / '.bash_profile',
        home / '.profile',
    ]
    for p in candidates:
        if p.exists():
            return p
    # Default to .bashrc
    return home / '.bashrc'


def find_zshrc(home: Path, zdotdir: Path | None = None) -> Path | None:
    """Find zsh config file."""
    if zdotdir:
        base = zdotdir
    else:
        base = home
    candidates = [
        base / '.zshrc',
        base / '.zshenv',
    ]
    for p in candidates:
        if p.exists():
            return p
    return None


def install_block(file: Path, block: str) -> bool:
    """Add managed block to file if not present. Returns True if changed."""
    content = file.read_text() if file.exists() else ''
    
    # Check if already installed
    if BLOCK_START in content and BLOCK_END in content:
        return False
    
    # Append block
    if content and not content.endswith('\n'):
        content += '\n'
    content += block
    
    file.write_text(content)
    return True


def remove_block(file: Path) -> bool:
    """Remove managed block from file. Returns True if changed."""
    if not file.exists():
        return False
    content = file.read_text()
    
    if BLOCK_START not in content:
        return False
    
    # Find and remove the block
    lines = content.split('\n')
    new_lines = []
    in_block = False
    for line in lines:
        if line.strip() == BLOCK_START:
            in_block = True
            continue
        if line.strip() == BLOCK_END:
            in_block = False
            continue
        if not in_block:
            new_lines.append(line)
    
    new_content = '\n'.join(new_lines)
    file.write_text(new_content)
    return True


def install_launcher(repo: Path, dry_run: bool = False) -> dict:
    """Install the global launcher."""
    home = Path.home()
    local_bin = home / '.local' / 'bin'
    launcher = local_bin / 'ai-run'
    target = repo / '.synthbit' / 'bin' / 'ai-run'
    
    result = {'action': 'install', 'launcher': str(launcher), 'created': False, 'updated': False}
    
    if dry_run:
        result['dry_run'] = True
        if launcher.exists():
            current = launcher.resolve()
            if str(current) != str(target.resolve()):
                result['would_update'] = True
            else:
                result['already_installed'] = True
        else:
            result['would_create'] = True
        return result
    
    local_bin.mkdir(parents=True, exist_ok=True)
    
    if launcher.exists():
        try:
            current_target = launcher.resolve()
            if str(current_target) == str(target.resolve()):
                result['already_installed'] = True
                return result
        except Exception:
            pass
        # Backup existing
        backup = launcher.with_suffix('.bak')
        shutil.copy2(launcher, backup)
        result['updated'] = True
    
    # Copy launcher
    shutil.copy2(target, launcher)
    launcher.chmod(0o755)
    result['created'] = True
    return result


def dry_run_install(repo: Path) -> dict:
    """Preview installation changes."""
    home = Path.home()
    bashrc = find_bashrc(home)
    zshrc = find_zshrc(home)
    
    launcher_info = install_launcher(repo, dry_run=True)
    
    bash_installed = False
    if bashrc and bashrc.exists():
        bash_installed = BLOCK_START in bashrc.read_text()
    
    zsh_installed = False
    if zshrc and zshrc.exists():
        zsh_installed = BLOCK_START in zshrc.read_text()
    
    return {
        'dry_run': True,
        'launcher': launcher_info,
        'bashrc': str(bashrc) if bashrc else None,
        'bash_installed': bash_installed,
        'zshrc': str(zshrc) if zshrc else None,
        'zsh_installed': zsh_installed,
        'activation': 'Run: source ~/.bashrc (or ~/.zshrc)'
    }


def do_install(repo: Path) -> dict:
    """Perform installation."""
    home = Path.home()
    bashrc = find_bashrc(home)
    zshrc = find_zshrc(home)
    
    results: dict[str, any] = {'bashrc': None, 'zshrc': None, 'launcher': None}
    
    if bashrc:
        changed = install_block(bashrc, BASHRC_SNIPPET)
        results['bashrc'] = {'path': str(bashrc), 'changed': changed}
    
    if zshrc and bashrc and zshrc != bashrc:
        changed = install_block(zshrc, BASHRC_SNIPPET)
        results['zshrc'] = {'path': str(zshrc), 'changed': changed}
    
    results['launcher'] = install_launcher(repo)
    results['activation'] = 'Run: source ~/.bashrc (or ~/.zshrc) to activate in current shell'
    return results


def do_uninstall() -> dict:
    """Remove installation."""
    home = Path.home()
    bashrc = find_bashrc(home)
    zshrc = find_zshrc(home)
    launcher = home / '.local' / 'bin' / 'ai-run'
    
    results: dict[str, any] = {'bashrc': None, 'zshrc': None, 'launcher': None}
    
    if bashrc and bashrc.exists():
        changed = remove_block(bashrc)
        results['bashrc'] = {'path': str(bashrc), 'changed': changed}
    
    if zshrc and bashrc and zshrc != bashrc and zshrc.exists():
        changed = remove_block(zshrc)
        results['zshrc'] = {'path': str(zshrc), 'changed': changed}
    
    if launcher.exists():
        try:
            target = launcher.resolve()
            expected = Path.home() / 'workspace' / 'synthbit' / '.synthbit' / 'bin' / 'ai-run'
            if str(target) == str(expected.resolve()):
                launcher.unlink()
                results['launcher'] = {'path': str(launcher), 'removed': True}
            else:
                results['launcher'] = {'path': str(launcher), 'kept': True, 'reason': 'Not managed by this harness'}
        except Exception:
            results['launcher'] = {'path': str(launcher), 'kept': True, 'reason': 'Could not verify ownership'}
    
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['dry-run', 'install', 'uninstall'])
    args = parser.parse_args(argv)
    
    try:
        root = repo_root()
    except Exception:
        # Fallback: use current working directory
        root = Path.cwd()
    
    if args.command == 'dry-run':
        result = dry_run_install(root)
        import json
        print(json.dumps(result, indent=2))
        return 0
    
    elif args.command == 'install':
        result = do_install(root)
        import json
        print(json.dumps(result, indent=2))
        return 0
    
    elif args.command == 'uninstall':
        result = do_uninstall()
        import json
        print(json.dumps(result, indent=2))
        return 0
    
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
