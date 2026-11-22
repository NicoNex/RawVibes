# Lens profiles

The Lens section's profiles come from the [lensfun](https://lensfun.github.io/) database,
converted to JSON at build time (`tools/lensfun_db.py`). The database is licensed under
[CC-BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) by the lensfun contributors.
RawVibes's correction maths is a port of lensfun 0.3.4 (LGPL-3.0); `tools/lensfun_ref.c`
checks the port against the library (development only, not shipped).
