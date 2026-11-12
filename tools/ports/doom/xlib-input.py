# Patches doomgeneric's Xlib driver for X servers without the XKB
# extension (Tinyx): XkbKeycodeToKeysym gives NoSymbol for every key
# there, so keys are looked up with XLookupKeysym; the release of an
# auto-repeated key (followed at once by its press) is dropped, which
# XkbSetDetectableAutoRepeat would do; and Alt, Tab, Backspace, Pause and
# the function keys are mapped. Run with the driver's path.
import sys
p = sys.argv[1]
s = open(p).read()
if "PALLADIUM" in s:
    sys.exit(0)

def edit(old, new):
    global s
    assert s.count(old) == 1, old[:60]
    s = s.replace(old, new)

edit("""    case XK_Shift_L:
    case XK_Shift_R:
		key = KEY_RSHIFT;
		break;
""", """    case XK_Shift_L:
    case XK_Shift_R:
		key = KEY_RSHIFT;
		break;
    /* PALLADIUM: more keys */
    case XK_Alt_L:
    case XK_Alt_R:
    case XK_Meta_L:
    case XK_Meta_R:
		key = KEY_RALT;
		break;
    case XK_Tab:
		key = KEY_TAB;
		break;
    case XK_BackSpace:
    case XK_Delete:
		key = KEY_BACKSPACE;
		break;
    case XK_Pause:
		key = KEY_PAUSE;
		break;
    case XK_F1: key = KEY_F1; break;
    case XK_F2: key = KEY_F2; break;
    case XK_F3: key = KEY_F3; break;
    case XK_F4: key = KEY_F4; break;
    case XK_F5: key = KEY_F5; break;
    case XK_F6: key = KEY_F6; break;
    case XK_F7: key = KEY_F7; break;
    case XK_F8: key = KEY_F8; break;
    case XK_F9: key = KEY_F9; break;
    case XK_F10: key = KEY_F10; break;
    case XK_F11: key = KEY_F11; break;
    case XK_F12: key = KEY_F12; break;
""")
edit("""	default:
		key = tolower(key);""", """	default:
		if (key > 0x7e)
			return 0; /* PALLADIUM: not a DOOM key */
		key = tolower(key);""")
edit("""                KeySym sym = XkbKeycodeToKeysym(s_Display, e.xkey.keycode, 0, 0);
                //printf("KeyPress:%d sym:%d\\n", e.xkey.keycode, sym);

                addKeyToQueue(1, sym);""", """                KeySym sym = XLookupKeysym(&e.xkey, 0);
                if (convertToDoomKey(sym))
                    addKeyToQueue(1, sym);""")
edit("""                KeySym sym = XkbKeycodeToKeysym(s_Display, e.xkey.keycode, 0, 0);
                //printf("KeyRelease:%d sym:%d\\n", e.xkey.keycode, sym);
                addKeyToQueue(0, sym);""", """                KeySym sym = XLookupKeysym(&e.xkey, 0);
                /* An auto-repeat: this release and the next press, the same key at the same time. */
                if (XEventsQueued(s_Display, QueuedAfterReading))
                {
                    XEvent next;
                    XPeekEvent(s_Display, &next);
                    if (next.type == KeyPress && next.xkey.keycode == e.xkey.keycode &&
                        next.xkey.time - e.xkey.time < 2)
                    {
                        XNextEvent(s_Display, &next);
                        continue;
                    }
                }
                if (convertToDoomKey(sym))
                    addKeyToQueue(0, sym);""")
open(p, "w").write(s)
