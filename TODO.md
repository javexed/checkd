# To do

Agents (`checkd-agent`, with nuthatch serving the binaries):

- **Signed binaries.** An agent updates itself only to a binary signed
  by a key it was installed with, not just one whose checksum nuthatch
  gives. Needed before `update = "auto"` is safe where it matters:
  today whoever can change nuthatch's agent directory can change every
  agent.
- **Going back.** A new binary that passes `--version` but then cannot
  connect is replaced by the one before it, instead of leaving the host
  without an agent until it is installed again.
- **Windows.** Agents on Windows update themselves as on Linux; today
  they are installed again.
