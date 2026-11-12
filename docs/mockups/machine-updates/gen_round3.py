"""Writes service/round3.html: machines always in panels, the aligned Machines dialog, compact menus,
and the update card for Local and for a server. One view per #hash."""

import pathlib
import sys

OUT = pathlib.Path(sys.argv[1])


def svg(paths, size=16):
    return (f'<svg width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" '
            f'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{paths}</svg>')


CLOSE = '<path d="M6 6l12 12"></path><path d="M18 6L6 18"></path>'
PLUS = '<path d="M12 5v14"></path><path d="M5 12h14"></path>'
BELL = ('<path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"></path>'
        '<path d="M13.73 21a2 2 0 0 1-3.46 0"></path>')
SEARCH = '<circle cx="11" cy="11" r="7"></circle><path d="M20 20l-3.5-3.5"></path>'
PERSON = '<circle cx="12" cy="8" r="4"></circle><path d="M5 21a7 7 0 0 1 14 0"></path>'
SERVER = ('<rect x="3" y="4" width="18" height="7" rx="2"></rect><rect x="3" y="13" width="18" height="7" rx="2"></rect>'
          '<path d="M7 7.5h.01M7 16.5h.01"></path>')
LAPTOP = '<rect x="4" y="5" width="16" height="11" rx="1.5"></rect><path d="M2 19h20"></path>'
GEAR = ('<path d="M9.8 4.7 L10.4 2.1 L13.6 2.1 L14.2 4.7 L15.6 5.3 L17.9 3.9 L20.1 6.1 L18.7 8.4 L19.3 9.8 L21.9 10.4 '
        'L21.9 13.6 L19.3 14.2 L18.7 15.6 L20.1 17.9 L17.9 20.1 L15.6 18.7 L14.2 19.3 L13.6 21.9 L10.4 21.9 L9.8 19.3 '
        'L8.4 18.7 L6.1 20.1 L3.9 17.9 L5.3 15.6 L4.7 14.2 L2.1 13.6 L2.1 10.4 L4.7 9.8 L5.3 8.4 L3.9 6.1 L6.1 3.9 L8.4 5.3Z">'
        '</path><circle cx="12" cy="12" r="3"></circle>')
CHEV = '<path d="M6 9l6 6 6-6"></path>'
CHEV_R = '<path d="M9 6l6 6-6 6"></path>'
DOTS = '<circle cx="5" cy="12" r="1.2"></circle><circle cx="12" cy="12" r="1.2"></circle><circle cx="19" cy="12" r="1.2"></circle>'
ARROW = '<path d="M5 12h14"></path><path d="M13 6l6 6-6 6"></path>'
CHECK = '<path d="M5 12l5 5 9-10"></path>'
CIRCLE = '<circle cx="12" cy="12" r="9"></circle>'
INFO = '<circle cx="12" cy="12" r="9"></circle><path d="M12 11v5"></path><path d="M12 8h.01"></path>'
MAP = ('<circle cx="6" cy="6" r="2.5"></circle><circle cx="18" cy="6" r="2.5"></circle>'
       '<circle cx="12" cy="18" r="2.5"></circle><path d="M7.5 8l3.5 7.5"></path><path d="M16.5 8L13 15.5"></path>')
SEND = '<path d="M12 19V5"></path><path d="M6 11l6-6 6 6"></path>'
BOLT = '<path d="M13 3L5 14h6l-1 7 8-11h-6z"></path>'


def row(title, status, activity, time):
    empty = " empty" if not activity else ""
    return (f'<div class="row-wrap"><button class="agent-row {status}"><span class="row-line"><span class="status-dot"></span>'
            f'<span class="row-title">{title}</span><span class="row-activity{empty}">{activity}</span>'
            f'<span class="row-time">{time}</span></span></button></div>')


def status_line(kind, text, link):
    button = f'<button class="link-button">{link}</button>' if link else ""
    return f'<p class="machine-trouble status-{kind}"><span class="status-mark"></span><span>{text}</span>{button}</p>'


def group(name, icon, count, line, rows, down=False):
    cls = "machine-group down" if down else "machine-group"
    return (f'<div class="{cls}"><button class="machine-head"><span class="machine-chevron">{svg(CHEV, 12)}</span>'
            f'<span class="machine-icon">{svg(icon, 14)}</span><span class="machine-name">{name}</span>'
            f'<span class="machine-count">{count}</span></button>{line}{rows}</div>')


def sidebar(body):
    return f'''
<nav class="sidebar">
  <div class="sidebar-head"><span class="app-name">Agents Cockpit</span>
    <button class="icon-button">{svg(BELL)}</button><button class="icon-button">{svg(SEARCH)}</button>
    <button class="icon-button raised">{svg(PLUS)}</button></div>
  <div class="agent-list scroll">{body}</div>
  <div class="sidebar-foot"><div class="foot-buttons">
    <button class="foot-accounts">{svg(PERSON, 13)}Accounts</button>
    <button class="foot-accounts">{svg(SERVER, 13)}Machines</button>
    <span class="spacer"></span>
    <button class="foot-accounts foot-settings">{svg(GEAR, 15)}</button></div>
  </div>
</nav>'''


LOCAL_ROWS = (row("Parser error columns", "status-working", "Running cargo test", "4 min")
              + row("Invoice export", "status-needs-you", "Waits for approval: Bash", "1 min")
              + row("Release notes", "status-idle", "", "1 h"))
ALONE_UPDATE = sidebar(group("Local", LAPTOP, 3, status_line("update", "Update available.", "Update"), LOCAL_ROWS))
ALONE_FINE = sidebar(group("Local", LAPTOP, 3, "", LOCAL_ROWS))
MANY = sidebar(
    group("Local", LAPTOP, 3, status_line("update", "Update available.", "Update"), LOCAL_ROWS)
    + group("devbox", SERVER, 2, "", row("Docs preview", "status-working", "Edit docs/index.md", "12 s")
            + row("Find the slow query", "status-idle", "", "12 min"))
    + group("gpu-box", SERVER, 1, status_line("down", "Not connected.", "Retry now"),
            row("Train the ranking model", "status-exited", "", "2 h"), down=True)
    + group("lab", SERVER, 1, status_line("down", "Runs an older version.", "Update"),
            row("Profile the data loader", "status-exited", "", "1 d"), down=True)
)


def chip(kind, text):
    return f'<span class="machine-chip chip-{kind}"><span class="chip-dot"></span>{text}</span>'


def card(icon, name, place, agents, state_chip, version, action, menu=True, note=""):
    note_html = f'<span class="machine-note">{note}</span>' if note else ""
    menu_html = f'<button class="icon-button small">{svg(DOTS, 16)}</button>' if menu else "<span></span>"
    return f'''
<div class="machine-line">
  <span class="machine-line-icon">{svg(icon, 18)}</span>
  <div class="machine-line-main"><span class="machine-line-name">{name}</span>
    <span class="machine-line-detail">{place} · {agents}</span>{note_html}</div>
  <span class="machine-line-state">{state_chip}</span>
  <span class="machine-line-version mono">{version}</span>
  <span class="machine-line-action">{action}</span>
  {menu_html}
</div>'''


UPDATE_BUTTON = '<button class="button small line-button primary">Update</button>'
RETRY_BUTTON = '<button class="button small line-button">Retry now</button>'

DIALOG_BODY = f'''
  <div class="dialog-head"><h2>Machines</h2><button class="icon-button small">{svg(CLOSE, 14)}</button></div>
  <div class="dialog-body">
    <div class="machine-table">
      <div class="machine-columns"><span></span><span>Machine</span><span>Status</span><span>Version</span><span></span><span></span></div>
      {card(LAPTOP, "Local", "Background service", "3 agents", chip("update", "Update available"), '<span class="old">0.1.0</span>' + svg(ARROW, 12) + "0.2.0", UPDATE_BUTTON, menu=False)}
      {card(SERVER, "devbox", "SSH devbox", "2 agents", chip("ok", "Connected"), '<span class="same">0.2.0</span>', "")}
      {card(SERVER, "gpu-box", "SSH gpu-box", "1 agent", chip("down", "Not connected"), "", RETRY_BUTTON, note="Retrying in 8 s.")}
      {card(SERVER, "lab", "SSH lab", "1 agent", chip("down", "Older version"), '<span class="old">0.1.0</span>' + svg(ARROW, 12) + "0.2.0", UPDATE_BUTTON)}
    </div>
    <button class="add-machine">{svg(PLUS, 14)}Add a server</button>
  </div>'''

COMPACT_MORE = '''
<div class="popover menu row-menu compact-more" style="--menu-x: 700px; --menu-y: 290px">
  <button class="menu-item"><span class="menu-label">Copy the SSH host</span></button>
  <div class="menu-divider"></div>
  <button class="menu-item danger"><span class="menu-label">Remove from the app</span></button>
</div>'''


def composer_menus():
    models = "".join(
        f'<button class="menu-item{" chosen" if chosen else ""}"><span class="menu-label">{name}</span>'
        f'{svg(CHECK, 14) if chosen else ""}</button>'
        for name, chosen in [("Opus 5.5", True), ("Sonnet 5.5", False), ("Haiku 5.5", False), ("Fable 5.1", False)]
    )
    efforts = "".join(f'<button class="effort{" chosen" if chosen else ""}">{label}</button>'
                      for label, chosen in [("Low", False), ("Med", False), ("High", False), ("XHigh", True), ("Max", False)])
    modes = "".join(
        f'<button class="menu-item compact-two{" chosen" if chosen else ""}"><span class="menu-label">{name}</span>'
        f'<span class="menu-sub">{text}</span></button>'
        for name, text, chosen in [
            ("Ask first", "Asks before edits and commands", False),
            ("Accept edits", "Edits files without asking, asks before commands", False),
            ("Plan", "Reads and plans, changes nothing", False),
            ("Auto", "A classifier approves most actions, asks for the rest", True),
            ("Bypass", "Runs everything without asking", False),
        ]
    )
    return f'''
<div class="menus-now-next">
  <div class="menu-col"><h3>Model menu, compact</h3>
    <div class="popover menu row-menu static-menu" style="width: 260px">
      <span class="menu-title">Model</span>{models}
      <div class="menu-divider"></div><span class="menu-title">Effort</span>
      <div class="effort-row">{efforts}</div>
    </div></div>
  <div class="menu-col"><h3>Mode menu, compact: the description stays, smaller</h3>
    <div class="popover menu row-menu static-menu" style="width: 320px">{modes}</div></div>
  <div class="menu-col"><h3>A server's menu in the Machines dialog</h3>
    <div class="popover menu row-menu static-menu" style="width: 220px">
      <button class="menu-item"><span class="menu-label">Copy the SSH host</span></button>
      <div class="menu-divider"></div>
      <button class="menu-item danger"><span class="menu-label">Remove from the app</span></button>
    </div></div>
</div>'''


def versions(left_name, left_version, left_commit, left_protocol, right_version, right_commit, right_protocol):
    return f'''
<div class="version-pair">
  <div class="version-box old-box"><span class="version-who">{left_name}</span>
    <span class="version-number">{left_version}</span><span class="version-meta mono">{left_commit} · protocol {left_protocol}</span></div>
  <span class="version-arrow">{svg(ARROW, 18)}</span>
  <div class="version-box"><span class="version-who">This app</span>
    <span class="version-number">{right_version}</span><span class="version-meta mono">{right_commit} · protocol {right_protocol}</span></div>
</div>'''


def update_card(icon, name, what):
    return f'''
<div class="update-card">
  <div class="update-card-head"><span class="update-card-icon">{svg(icon, 20)}</span>
    <div><h1>{name} runs an older version</h1>
    <p class="update-card-lead">The app can reach {name}'s agents again once {name} runs this app's version.</p></div></div>
  {versions(name, "0.1.0", "f00ba12", 8, "0.2.0", "a1b2c3d", 9)}
  <div class="update-callout"><span class="callout-icon">{svg(INFO, 16)}</span><span>{what}</span></div>
  <div class="update-card-actions">
    <button class="details-link">{svg(CHEV_R, 12)}Details</button>
    <span class="spacer"></span>
    <button class="button">Retry</button>
    <button class="button primary">Update {name}</button>
  </div>
</div>'''


def step(label, state):
    mark = {"done": svg(CHECK, 14), "now": '<span class="spinner"></span>', "": svg(CIRCLE, 12)}[state]
    return f'<li class="{state}"><span class="update-mark">{mark}</span>{label}</li>'


def progress_card(icon, name, steps, lead):
    rows = "".join(step(label, state) for label, state in steps)
    return f'''
<div class="update-card">
  <div class="update-card-head"><span class="update-card-icon">{svg(icon, 20)}</span>
    <div><h1>Updating {name}</h1><p class="update-card-lead">{lead}</p></div></div>
  <ol class="update-steps">{rows}</ol>
</div>'''


LOCAL_WHAT = "The update restarts Local's background service with this app's program. A working agent stops its current turn, and every agent resumes its conversation after."
SERVER_WHAT = "The update builds 0.2.0 on lab from its checkout, then restarts its daemon. A working agent stops its current turn, and every agent resumes its conversation after."

CSS = """
html, body { height: auto; overflow: visible; background: #0c0c0c; }
body { padding: 16px; }
section { display: none; }
section.shown { display: flex; gap: 24px; align-items: flex-start; flex-wrap: wrap; }
*, *::before, *::after { animation: none !important; transition: none !important; }
.lone { display: flex; flex-direction: column; gap: 8px; }
.lone h3, .menu-col h3 { margin: 0; font-size: 13px; color: var(--muted); font-weight: 600; }
.lone .sidebar { flex: 0 0 270px; width: 270px; max-width: none; height: 700px; border: 1px solid #2a2a2a; border-radius: 10px; }
.screen { position: relative; width: 1100px; height: 600px; border: 1px solid #2a2a2a; border-radius: 10px; overflow: hidden; background: var(--bg); display: flex; }
.screen .sidebar { flex: 0 0 270px; width: 270px; max-width: none; }
.center-blank { flex: 1 1 auto; }

.machine-trouble { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
.status-mark { width: 7px; height: 7px; border-radius: 50%; flex: none; }
.status-update .status-mark { background: var(--amber); }
.status-down .status-mark { background: var(--red); }

/* The Machines dialog: one row per machine, its parts in columns. */
.machines-dialog { width: 680px; position: relative; overflow: visible; }
.machine-table { display: flex; flex-direction: column; gap: 4px; }
.machine-columns, .machine-line { display: grid; grid-template-columns: 22px minmax(0, 1fr) 132px 112px 92px 32px; align-items: center; column-gap: 12px; }
.machine-columns { padding: 0 8px 2px 12px; font-size: 11.5px; color: var(--dim); }
.machine-line { min-height: 54px; padding: 8px 8px 8px 12px; border-radius: 10px; background: var(--raised); }
.machine-line-icon { display: inline-flex; color: var(--muted); }
.machine-line-main { min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.machine-line-name { font-weight: 600; font-size: 14px; }
.machine-line-detail { font-size: 12.5px; color: var(--muted); }
.machine-note { font-size: 12px; color: var(--dim); }
.machine-chip { display: inline-flex; align-items: center; gap: 5px; height: 20px; padding: 0 7px; border-radius: 999px; font-size: 11.5px; white-space: nowrap; }
.chip-dot { width: 6px; height: 6px; border-radius: 50%; }
.chip-ok { background: rgba(74, 194, 107, 0.12); color: #8fd3a8; }
.chip-ok .chip-dot { background: var(--green); }
.chip-update { background: rgba(245, 165, 36, 0.14); color: #f5c06a; }
.chip-update .chip-dot { background: var(--amber); }
.chip-down { background: rgba(244, 112, 103, 0.14); color: #f6a49c; }
.chip-down .chip-dot { background: var(--red); }
.machine-line-version { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; color: var(--text-soft); }
.machine-line-version .old { color: var(--dim); }
.machine-line-version .same { color: var(--muted); }
.line-button { width: 92px; justify-content: center; }
.add-machine { align-self: flex-start; display: inline-flex; align-items: center; gap: 6px; height: 32px; padding: 0 10px; border: 0; border-radius: 8px; background: transparent; color: var(--text-soft); font-size: 13px; }
.popover.row-menu.compact-more { z-index: 30; width: 220px; }
.menu-item.danger { color: var(--red); }

/* Compact menus, as the agent row's. */
.menus-now-next { display: flex; gap: 32px; align-items: flex-start; }
.menu-col { display: flex; flex-direction: column; gap: 8px; }
.popover.row-menu.static-menu { position: static; }
.row-menu .menu-title { padding: 6px 8px 2px; }
.row-menu .menu-item.compact-two { flex-direction: column; align-items: flex-start; justify-content: center; gap: 0; min-height: 0; padding: 4px 8px; white-space: normal; }
.menu-sub { font-size: 11.5px; color: var(--dim); line-height: 1.3; }
.row-menu .effort-row { padding: 2px 4px 4px; }

/* The update card, for Local and for a server. */
.update-card { width: 560px; display: flex; flex-direction: column; gap: 16px; padding: 22px 24px 20px; border-radius: 14px; background: var(--panel); box-shadow: var(--shadow); }
.update-card-head { display: flex; gap: 14px; align-items: flex-start; }
.update-card-icon { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 38px; height: 38px; border-radius: 10px; background: var(--raised); color: var(--text-soft); }
.update-card h1 { margin: 0; font-size: 18px; }
.update-card-lead { margin: 4px 0 0; color: var(--muted); font-size: 13.5px; }
.version-pair { display: grid; grid-template-columns: 1fr auto 1fr; align-items: center; gap: 10px; }
.version-box { display: flex; flex-direction: column; gap: 2px; padding: 10px 12px; border-radius: 10px; background: var(--raised); }
.version-box.old-box { background: rgba(244, 112, 103, 0.08); }
.version-who { font-size: 12px; color: var(--dim); }
.version-number { font-size: 16px; font-weight: 600; }
.version-meta { font-size: 11.5px; color: var(--muted); }
.version-arrow { display: inline-flex; color: var(--dim); }
.update-callout { display: flex; gap: 10px; padding: 10px 12px; border-radius: 10px; background: rgba(255, 255, 255, 0.04); color: var(--text-soft); font-size: 13px; }
.callout-icon { flex: none; display: inline-flex; color: var(--muted); padding-top: 1px; }
.update-card-actions { display: flex; align-items: center; gap: 8px; }
.details-link { display: inline-flex; align-items: center; gap: 4px; padding: 0; border: 0; background: none; color: var(--muted); font-size: 12.5px; }
.update-card .update-steps { margin: 0; }
"""

SECTIONS = {
    "panels": f'<div class="lone"><h3>Local alone, an update available</h3>{ALONE_UPDATE}</div>'
              f'<div class="lone"><h3>Local alone, up to date</h3>{ALONE_FINE}</div>'
              f'<div class="lone"><h3>Four machines</h3>{MANY}</div>',
    "dialog": f'<div class="screen">{MANY}<div class="center-blank"></div><div class="overlay"><div class="dialog machines-dialog">{DIALOG_BODY}{COMPACT_MORE}</div></div></div>',
    "menus": composer_menus(),
    "cards": update_card(LAPTOP, "Local", LOCAL_WHAT) + update_card(SERVER, "lab", SERVER_WHAT),
    "progress": progress_card(LAPTOP, "Local", [("Stop version 0.1.0", "done"), ("Start version 0.2.0 (a1b2c3d)", "now"), ("Reconnect", "")],
                              "The agents resume once Local runs version 0.2.0.")
                + progress_card(SERVER, "lab", [("Get version a1b2c3d", "done"), ("Build it on lab", "now"), ("Restart the daemon", ""), ("Reconnect", "")],
                                "The daemon keeps running until the new version is built."),
}
body = "".join(f'<section id="{key}">{html}</section>' for key, html in SECTIONS.items())
JS = "document.getElementById(location.hash.slice(1) || 'panels').classList.add('shown');"
OUT.write_text(f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="style.css">'
               f'<style>{CSS}</style></head><body>{body}<script>{JS}</script></body></html>', encoding="utf-8")
print("wrote", OUT)
