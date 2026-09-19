# Security history

A live Telegram bot token was historically committed to this public repository.
The current tracked tree no longer contains that literal and CI scans for the
token shape, but deletion from the current tree does not revoke historical
copies.

Required operational remediation:
1. revoke/rotate the affected token at Telegram/BotFather;
2. store only the replacement in the server's untracked `.env`;
3. verify the single poller and alert delivery;
4. optionally coordinate a Git-history rewrite after rotation.

Credential rotation is the security boundary. History rewriting without
rotation is not sufficient.
