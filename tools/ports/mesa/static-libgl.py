# Copyright (c) 2026 tre4surehunter9
# Mesa's xlib libGL target (src/gallium/targets/libgl-xlib/meson.build) as
# a static library: shared_library -> static_library, without the
# arguments only a shared library takes (link arguments, version script,
# versions). tools/ports/mesa.sh then merges it with Mesa's other static
# libraries into one libGL.a.
import re
import sys

path = sys.argv[1]
s = open(path).read()
if "static_library(\n  'GL'" not in s:
    assert "libgl = shared_library(\n  'GL'," in s, "libgl-xlib's meson.build changed"
    s = s.replace("libgl = shared_library(\n  'GL',", "libgl = static_library(\n  'GL',")
    for key in ("link_args", "link_depends", "version", "darwin_versions"):
        s, n = re.subn(r"\n  %s\s*:[^\n]*" % key, "", s, count=1)
        assert n == 1, key
open(path, "w").write(s)
