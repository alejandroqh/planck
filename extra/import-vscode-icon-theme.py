#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["fonttools"]
# ///
"""Import a font based VS Code file icon theme (such as Monokai Pro's) into Planck.

Each glyph becomes a colored SVG, and the theme's file names, extensions and
language ids become Planck icon mappings. Nothing is bundled with Planck: run it
on your own copy of the extension.

Usage: uv run extra/import-vscode-icon-theme.py EXTENSION THEMES_DIR [LABEL]
  EXTENSION   a .vsix file or an unpacked extension folder
  THEMES_DIR  Planck's themes folder, e.g. ~/.local/share/planck-nightly/themes
  LABEL       which of the extension's icon themes to import (default: the first)
Then pick the theme under Settings > Core > Icon Theme.
"""

import io
import json
import posixpath
import re
import sys
import tomllib
import zipfile
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont

# VS Code gives these languages their files itself, so icon themes map them by
# language id only. Planck has no language ids, so list the usual files here.
LANGUAGE_EXTENSIONS = {
    "bat": ["bat", "cmd"],
    "c": ["c", "i"],
    "clojure": ["clj", "cljs", "cljc", "edn"],
    "coffeescript": ["coffee"],
    "cpp": ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "h", "inl", "ipp"],
    "csharp": ["cs", "csx"],
    "css": ["css"],
    "dart": ["dart"],
    "diff": ["diff", "patch"],
    "fsharp": ["fs", "fsi", "fsx"],
    "go": ["go"],
    "groovy": ["groovy", "gvy"],
    "handlebars": ["handlebars", "hbs"],
    "html": ["html", "htm", "xhtml"],
    "ini": ["ini"],
    "java": ["java", "jav"],
    "javascript": ["js", "mjs", "cjs", "es6"],
    "javascriptreact": ["jsx"],
    "json": ["json", "webmanifest"],
    "jsonc": ["jsonc"],
    "julia": ["jl"],
    "less": ["less"],
    "lua": ["lua"],
    "makefile": ["mk"],
    "markdown": ["md", "markdown", "mdown"],
    "perl": ["pl", "pm", "t"],
    "php": ["php", "phtml"],
    "plaintext": ["txt"],
    "powershell": ["ps1", "psm1", "psd1"],
    "python": ["py", "pyi", "pyw"],
    "r": ["r"],
    "ruby": ["rb", "gemspec", "rake"],
    "rust": ["rs"],
    "scss": ["scss"],
    "shellscript": ["sh", "bash", "zsh", "fish", "ksh"],
    "sql": ["sql"],
    "swift": ["swift"],
    "typescript": ["ts", "mts", "cts"],
    "typescriptreact": ["tsx"],
    "xml": ["xml", "xsd", "xsl", "xslt", "svg", "plist"],
    "yaml": ["yaml", "yml"],
}
# Lowercase, as Planck falls back to lowercase names.
LANGUAGE_FILENAMES = {
    "dockerfile": ["dockerfile", "containerfile"],
    "makefile": ["makefile", "gnumakefile"],
    "ruby": ["gemfile", "rakefile"],
}
# Planck UI icons that file icon themes also draw. Folders go on the expand
# arrows: themes like Monokai Pro draw them as arrows and hide VS Code's own.
UI_ICONS = {
    "file": "file",
    "item.closed": "folder",
    "item.opened": "folderExpanded",
}


def file_stem(icon):
    """A safe file name for an icon id, which comes from the downloaded theme."""
    return re.sub(r"[^\w-]", "_", icon)


def extension_reader(path):
    """A function reading files of an extension by their path from its root."""
    if path.is_dir():
        return lambda rel: (path / rel).read_bytes()
    vsix = zipfile.ZipFile(path)
    return lambda rel: vsix.read(posixpath.join("extension", rel))


def glyph_svgs(font, theme):
    """{icon id: svg text} for every icon definition, colored by its fontColor."""
    upm = font["head"].unitsPerEm
    cmap = font.getBestCmap()
    glyphs = font.getGlyphSet()
    svgs = {}
    for icon, definition in theme["iconDefinitions"].items():
        # shortcut: one font per theme and no iconPath icons, enough for Monokai Pro.
        char = int(definition["fontCharacter"].lstrip("\\"), 16)
        pen = SVGPathPen(glyphs)
        glyphs[cmap[char]].draw(pen)
        color = definition.get("fontColor", "currentColor")
        # Font outlines point y up; flip them into the em square.
        svgs[icon] = (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {upm} {upm}">'
            f'<path transform="matrix(1 0 0 -1 0 {upm})" fill="{color}" d="{pen.getCommands()}"/>'
            "</svg>\n"
        )
    return svgs


def mappings(theme):
    """Planck's filename and extension tables as {key: icon id}."""
    filename = dict(theme.get("fileNames", {}))
    # Planck matches the last extension only, so compound ones like "d.ts" never match.
    extension = {k: v for k, v in theme.get("fileExtensions", {}).items() if "." not in k}
    for language, icon in theme.get("languageIds", {}).items():
        for name in LANGUAGE_FILENAMES.get(language, []):
            filename.setdefault(name, icon)
        for ext in LANGUAGE_EXTENSIONS.get(language, []):
            extension.setdefault(ext, icon)
    return filename, extension


def toml_table(name, entries):
    # JSON strings are valid TOML basic strings.
    lines = [f"[icon-theme.{name}]"]
    lines += [f"{json.dumps(k)} = {json.dumps(v + '.svg')}" for k, v in entries.items()]
    return "\n".join(lines) + "\n"


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    read = extension_reader(Path(sys.argv[1]).expanduser())
    themes_dir = Path(sys.argv[2]).expanduser()

    contributed = json.loads(read("package.json"))["contributes"]["iconThemes"]
    labels = [t["label"] for t in contributed]
    label = sys.argv[3] if len(sys.argv) == 4 else labels[0]
    if label not in labels:
        sys.exit(f"No icon theme {label!r}; the extension has: {', '.join(labels)}")
    theme_path = posixpath.normpath(contributed[labels.index(label)]["path"])
    theme = json.loads(read(theme_path))

    font_path = theme["fonts"][0]["src"][0]["path"]
    font_path = posixpath.normpath(posixpath.join(posixpath.dirname(theme_path), font_path))
    svgs = glyph_svgs(TTFont(io.BytesIO(read(font_path))), theme)

    out = themes_dir / re.sub(r"[^a-z0-9]+", "-", label.lower()).strip("-")
    (out / "icons").mkdir(parents=True, exist_ok=True)
    for icon, svg in svgs.items():
        (out / "icons" / f"{file_stem(icon)}.svg").write_text(svg)

    filename, extension = mappings(theme)
    ui = {k: theme[v] for k, v in UI_ICONS.items() if v in theme}
    toml = (
        f"[icon-theme]\nname = {json.dumps(label)}\n\n"
        + "\n".join(
            toml_table(name, {k: f"icons/{file_stem(v)}" for k, v in table.items()})
            for name, table in [("ui", ui), ("filename", filename), ("extension", extension)]
        )
    )
    (out / "icon-theme.toml").write_text(toml)

    # Self-check: the TOML parses and every icon it names exists.
    parsed = tomllib.loads(toml)["icon-theme"]
    for table in ("ui", "filename", "extension"):
        for icon in parsed[table].values():
            assert (out / icon).is_file(), icon
    print(
        f"Imported {label!r} into {out}: {len(svgs)} icons, "
        f"{len(filename)} file names, {len(extension)} extensions"
    )


if __name__ == "__main__":
    main()
