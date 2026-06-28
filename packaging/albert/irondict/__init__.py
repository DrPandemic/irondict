# -*- coding: utf-8 -*-
"""Search the irondict dictionaries from Albert.

The plugin exposes several query handlers:

- A general handler (trigger ``d `` by default) that searches every dictionary.
- One handler per dictionary that has a pinned language (set it in irondict's
  Settings), keyed by the language code so you get per-language shortcuts, e.g.
  ``df `` for a French dictionary or ``di `` for an Italian one. Rebind any of
  them from Albert's settings.

Type a trigger followed by a word to list every matching headword; activating a
result opens the irondict GUI straight to that word's definition, scoped to the
dictionary the result came from.
"""

import re
import shutil
import subprocess
from time import sleep

from albert import *

md_iid = "5.0"
md_version = "1.1"
md_name = "IronDict"
md_description = "Look up words in the irondict dictionaries"
md_license = "GPL-3.0-or-later"
md_url = "https://github.com/DrPandemic/irondict"
md_authors = ["@DrPandemic"]
md_bin_dependencies = ["irondict"]

# Parses one `irondict search` result line:
# "Headword  [dictionary]  (score 1.00)\tsnippet".
# The headword may contain spaces, so match up to the two-space + "[dict]" marker.
# The snippet (separated by \t) is extracted separately in _search().
RESULT_RE = re.compile(r"^(.+?)\s+\[([^\]]+)\]\s+\(score")

# Parses one `irondict list` line:
# "Name [enabled] [fr] — 123 words (/path/to.ifo)".
LIST_RE = re.compile(r"^(.+?) \[(enabled|disabled)\] \[([a-z]+)\] .* \((.+)\)\s*$")


class DictHandler(GeneratorQueryHandler):
    """A query handler over the irondict dictionaries.

    With ``dict_name`` set, both the search and the GUI it opens are scoped to
    that single dictionary, so a trigger can target one language. With it
    ``None`` the handler searches across every dictionary.
    """

    def __init__(self, plugin, ext_id, name, description, trigger, dict_name):
        GeneratorQueryHandler.__init__(self)
        self._plugin = plugin
        self._id = ext_id
        self._name = name
        self._description = description
        self._trigger = trigger
        self._dict = dict_name  # None = search all dictionaries.
        self._fuzzy = bool(plugin.readConfig(self._fuzzy_key(), bool))

    # --- extension identity --------------------------------------------------

    def id(self):
        return self._id

    def name(self):
        return self._name

    def description(self):
        return self._description

    def defaultTrigger(self):
        return self._trigger

    def synopsis(self, query):
        return "word"

    # --- fuzzy matching (per handler, persisted by id) -----------------------

    def _fuzzy_key(self):
        return f"fuzzy/{self._id}"

    def supportsFuzzyMatching(self):
        return True

    def setFuzzyMatching(self, enabled):
        self._fuzzy = enabled
        self._plugin.writeConfig(self._fuzzy_key(), enabled)

    # --- items ---------------------------------------------------------------

    def _open_action(self, headword):
        # Open the GUI on this word, scoped to the dictionary it came from so the
        # definition shown matches the result the user picked.
        cmd = [self._plugin.executable, "--gui", "--word", headword]
        if self._dict:
            cmd += ["--dict", self._dict]
        return Action("open", "Open in IronDict", lambda c=cmd: runDetachedProcess(c))

    def _item(self, headword, dictionary, snippet):
        return StandardItem(
            id=headword,
            text=headword,
            subtext=snippet,
            icon_factory=self._plugin.icon,
            actions=[
                self._open_action(headword),
                Action("copy", "Copy word", lambda hw=headword: setClipboardText(hw)),
            ],
        )

    def _search(self, query):
        mode = "fuzzy" if self._fuzzy else "prefix"
        cmd = [self._plugin.executable, "search", query, "--mode", mode, "--limit", "25", "--with-snippet"]
        if self._dict:
            cmd += ["--dict", self._dict]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        except (subprocess.TimeoutExpired, OSError) as e:
            warning(f"irondict search failed: {e}")
            return []

        items = []
        for line in proc.stdout.splitlines():
            m = RESULT_RE.match(line)
            if m:
                snippet = ""
                if "\t" in line:
                    snippet = line.rsplit("\t", 1)[-1]
                items.append(self._item(m.group(1), m.group(2), snippet))
        return items

    def items(self, ctx):
        query = ctx.query.strip()

        if not query:
            yield [
                StandardItem(
                    id=self._id,
                    text=self._name,
                    subtext="Type a word to search",
                    icon_factory=self._plugin.icon,
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
                    id=self._id,
                    text=f"No matches for “{query}”",
                    subtext="Open IronDict to search anyway",
                    icon_factory=self._plugin.icon,
                    actions=[self._open_action(query)],
                )
            ]


def _is_companion(path):
    """A hidden companion dictionary (e.g. the `fr-conj` conjugation table) lives
    in a `*-conj` directory and is auto-paired with its primary; it isn't a
    standalone dictionary the user would scope a trigger to."""
    return "-conj/" in path.replace("\\", "/")


def _dedup(base, used):
    """`base`, or `base` with the lowest free numeric suffix (e.g. a second
    French dictionary becomes `irondict.fr2`)."""
    if base not in used:
        return base
    n = 2
    while f"{base}{n}" in used:
        n += 1
    return f"{base}{n}"


def _lang_trigger(lang, used):
    """A short default trigger for a language: ``d`` + the language's first
    letter (``df ``, ``de ``, ``di ``), falling back to the full code (``dfr ``)
    then a number when a language has several dictionaries. The user can rebind
    it in Albert's settings; this only needs to be unique and stable."""
    for candidate in (f"d{lang[0]} ", f"d{lang} "):
        if candidate not in used:
            return candidate
    n = 2
    while f"d{lang}{n} " in used:
        n += 1
    return f"d{lang}{n} "


class Plugin(PluginInstance):

    def __init__(self):
        PluginInstance.__init__(self)
        self.executable = shutil.which("irondict")
        if not self.executable:
            raise RuntimeError("irondict executable not found in $PATH")
        self._handlers = self._build_handlers()

    def icon(self):
        return Icon.theme("irondict")

    def extensions(self):
        return self._handlers

    def _build_handlers(self):
        # The general handler keeps the plugin id so an existing custom `d `
        # trigger is preserved across upgrades.
        handlers = [
            DictHandler(
                self, "irondict", "IronDict",
                "Search every dictionary", "d ", None,
            )
        ]

        used_ids = {"irondict"}
        used_triggers = {"d "}
        for name, lang in self._dictionaries():
            ext_id = _dedup(f"irondict.{lang}", used_ids)
            used_ids.add(ext_id)
            trigger = _lang_trigger(lang, used_triggers)
            used_triggers.add(trigger)
            handlers.append(
                DictHandler(
                    self,
                    ext_id,
                    f"IronDict — {name}",
                    f"Search the {name} dictionary",
                    trigger,
                    name,
                )
            )
        return handlers

    def _dictionaries(self):
        """The enabled dictionaries with a pinned language, as ``(name, code)``
        pairs. Skips companions and dictionaries left on the ``auto`` language
        (they have no code to key a per-language handler on)."""
        try:
            proc = subprocess.run(
                [self.executable, "list"],
                capture_output=True, text=True, timeout=10,
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            warning(f"irondict list failed: {e}")
            return []

        dicts = []
        for line in proc.stdout.splitlines():
            m = LIST_RE.match(line)
            if not m:
                continue
            name, state, lang, path = m.group(1), m.group(2), m.group(3), m.group(4)
            if state == "enabled" and lang != "auto" and not _is_companion(path):
                dicts.append((name, lang))
        return dicts
