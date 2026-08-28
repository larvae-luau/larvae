#!/usr/bin/env python3
"""Give the vendored globalTypes.d.luau the shapes upstream has not shipped.

The nightly copies luau-lsp's generated file whole, and that file stubs a
datatype it does not know as `type Name = any`. Each patch here replaces one
stub with the shape its luau-lsp pull request declares, and does nothing
once upstream ships the real type: a missing stub means the patch is done
for good and this entry can go.
"""

import pathlib
import sys

FILE = pathlib.Path(__file__).resolve().parent.parent / (
    "crates/larvae-lsp/types/globalTypes.d.luau"
)

# luau-lsp pull 1532/1587: ScopedInstanceIdentity, from the setup-rbxcdn
# reference. Remove when the official dump carries the members.
PATCHES = [
    (
        "type ScopedInstanceIdentity = any",
        "declare extern type ScopedInstanceIdentity with\n"
        "\tfunction ResolveInstance(self, scope: Instance): Instance?\n"
        "end",
    ),
]


def main() -> int:
    text = FILE.read_text()
    applied = 0

    for stub, shape in PATCHES:
        if stub in text:
            text = text.replace(stub, shape, 1)
            applied += 1

    if applied:
        FILE.write_text(text)

    print(f"patched {applied} of {len(PATCHES)} stubs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
