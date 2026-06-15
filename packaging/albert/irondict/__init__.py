# -*- coding: utf-8 -*-
"""Search the irondict dictionaries from Albert.

Type the trigger (``d `` by default) followed by a word to list every matching
headword across the enabled dictionaries. Activating a result opens the irondict
GUI straight to that word's definition.
"""

import re
import shutil
import subprocess
from time import sleep

from albert import *

md_iid = "5.0"
md_version = "1.0"
md_name = "IronDict"
md_description = "Look up words in the irondict dictionaries"
md_license = "GPL-3.0-or-later"
md_url = "https://github.com/DrPandemic/irondict"
md_authors = ["@DrPandemic"]
md_bin_dependencies = ["irondict"]

# Parses one `irondict search` result line: "Headword  [dictionary]  (score 1.00)".
# The headword may contain spaces, so match up to the two-space + "[dict]" marker.
RESULT_RE = re.compile(r"^(.+?)\s+\[([^\]]+)\]\s+\(score")


class Plugin(PluginInstance, GeneratorQueryHandler):

    def __init__(self):
        PluginInstance.__init__(self)
        GeneratorQueryHandler.__init__(self)
        self.executable = shutil.which("irondict")
        if not self.executable:
            raise RuntimeError("irondict executable not found in $PATH")
        # Prefix (autocomplete) matching by default; the user can switch to
        # typo-tolerant fuzzy matching from the plugin settings.
        self._fuzzy = self.readConfig("fuzzy", bool) or False

    def defaultTrigger(self):
        return "d "

    def synopsis(self, query):
        return "word"

    def supportsFuzzyMatching(self):
        return True

    def setFuzzyMatching(self, enabled):
        self._fuzzy = enabled
        self.writeConfig("fuzzy", enabled)

    @staticmethod
    def _icon():
        return Icon.theme("irondict")

    def _open_action(self, headword):
        return Action(
            "open",
            "Open in IronDict",
            lambda hw=headword: runDetachedProcess(
                [self.executable, "--gui", "--word", hw]
            ),
        )

    def _item(self, headword, dictionary):
        return StandardItem(
            id=headword,
            text=headword,
            subtext=dictionary,
            icon_factory=self._icon,
            actions=[
                self._open_action(headword),
                Action("copy", "Copy word", lambda hw=headword: setClipboardText(hw)),
            ],
        )

    def _search(self, query):
        mode = "fuzzy" if self._fuzzy else "prefix"
        try:
            proc = subprocess.run(
                [self.executable, "search", query,
                 "--mode", mode, "--limit", "25"],
                capture_output=True, text=True, timeout=10,
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            warning(f"irondict search failed: {e}")
            return []

        items = []
        for line in proc.stdout.splitlines():
            m = RESULT_RE.match(line)
            if m:
                items.append(self._item(m.group(1), m.group(2)))
        return items

    def items(self, ctx):
        query = ctx.query.strip()

        if not query:
            yield [
                StandardItem(
                    id=self.id(),
                    text=self.name(),
                    subtext="Type a word to search the dictionaries",
                    icon_factory=self._icon,
                )
            ]
            return

        # Debounce: wait briefly and bail if the user kept typing, so we don't
        # spawn an irondict process on every keystroke.
        for _ in range(15):
            sleep(0.01)
            if not ctx.isValid:
                return

        items = self._search(query)
        if not ctx.isValid:
            return

        if items:
            yield items
        else:
            yield [
                StandardItem(
                    id=self.id(),
                    text=f"No matches for “{query}”",
                    subtext="Open IronDict to search anyway",
                    icon_factory=self._icon,
                    actions=[self._open_action(query)],
                )
            ]
