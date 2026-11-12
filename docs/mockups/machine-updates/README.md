# Machine updates: the approved mockups

The user approved these screens for updating any machine's daemon. The data in them is made up.

- `r3-panels.png`: machine panels, with Local alone and with several machines.
- `r3-dialog.png`: the Machines dialog as a grid, with a server's ⋯ menu.
- `r3-menus.png`: the composer's model and mode menus, and the server menu, in the compact layout.
- `r3-cards.png`: the update card, for Local and for a server.
- `r3-progress.png`: the update under way, for Local and for a server.
- `confirm.png`: the confirmation when agents are busy. It comes from an earlier round, so it says "This computer", which is now "Local".

`gen_round3.py` writes the HTML of the round 3 screens. Its CSS block holds the styles the screens use. Run it with the output file as its argument, next to a copy of `crates/cockpit_desktop/src/style.css`, then open the file at `#panels`, `#dialog`, `#menus`, `#cards` or `#progress`.

Delete this folder before the branch is merged into `main`, unless the user wants to keep it.
