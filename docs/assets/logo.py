import shutil, sys
FULL = r"""          ▄███▄         ██████   ██████  ███    ███ ██████   ██████
          ██▄███▄      ██   ██ ██    ██ ████  ████ ██   ██ ██    ██          ▀▀
    ▄▄▄▄▄█████▀       ██████  ██    ██ ██ ████ ██ ██████  ██    ██          ██  ██▄▀▀▀  ▄█▀▀▀▀
▄▄████████████       ██      ██    ██ ██  ██  ██ ██   ██ ██    ██    ▀▀▀▀  ██  ██      ██
 ▀▀██████████▀      ██       ██████  ██      ██ ██████   ██████           ██  ██      ▀█▄▄▄▄
      █▄  █▄"""
COMPACT = r"""     ▄██▄                                  ██                      ▀▀
  ▄▄▄█▄██▀    ██▀▀█▄  ▄█▀▀█▄  ██▀▀██▀▀█▄  ██▀▀█▄  ▄█▀▀█▄   ▄▄▄▄  ██  ██▄▀▀▀  ▄█▀▀▀
▀███████▀    ██▄▄█▀  ▀█▄▄█▀  ██  ██  ██  ██▄▄█▀  ▀█▄▄█▀         ██  ██      ▀█▄▄▄
   █▄ █▄    ██"""
A, B = (0x7d, 0xd3, 0xc0), (0xc4, 0x9c, 0xf0)
DIM = "\x1b[38;2;103;112;135m"
def grad(art):
    w = max(map(len, art.split("\n")))
    for line in art.split("\n"):
        out = ""
        for i, ch in enumerate(line):
            c = [round(x + (y - x) * i / w) for x, y in zip(A, B)]
            out += f"\x1b[1;38;2;{c[0]};{c[1]};{c[2]}m{ch}"
        print(out + "\x1b[0m")
def chip():
    print("\x1b[1;48;2;125;211;192;38;2;16;18;26m pombo-irc \x1b[0m")
def show(cols):
    full_w = max(map(len, FULL.split("\n")))
    comp_w = max(map(len, COMPACT.split("\n")))
    if cols >= full_w + 2: grad(FULL)
    elif cols >= comp_w + 2: grad(COMPACT)
    else: chip()
    print(DIM + "  one-to-one  ·  end-to-end encrypted  ·  nothing stored\x1b[0m")
if "--all" in sys.argv:
    for name, cols in (("full", 200), ("compact", 90), ("narrow", 60)):
        print(DIM + f"── {name} ──\x1b[0m"); show(cols); print()
else:
    show(shutil.get_terminal_size().columns)
