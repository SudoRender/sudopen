# dmgbuild settings for the sudopen installer DMG (used by scripts/dmg.sh).
# Writes the Finder window layout directly, so no Finder scripting is needed.
import os.path

app = defines["app"]  # noqa: F821 (provided by dmgbuild)
app_name = os.path.basename(app)

format = "ULFO"  # lzfse, macOS 10.11+
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines.get("icon")  # noqa: F821  volume icon
background = defines["background"]  # noqa: F821

window_rect = ((200, 140), (660, 420))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
icon_size = 112
text_size = 13
icon_locations = {app_name: (180, 210), "Applications": (480, 210)}
