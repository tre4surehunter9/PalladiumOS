# Copyright (c) 2026 tre4surehunter9
# Checks that the MicroPython port works on PalladiumOS: the language, and
# the parts that depend on the OS. Prints "micropython checks passed".
import sys, os, time, json, re, struct, random, binascii, hashlib, errno

# the language
class Point:
    def __init__(self, x, y):
        self.x, self.y = x, y
    def __add__(self, o):
        return Point(self.x + o.x, self.y + o.y)
p = Point(1, 2) + Point(3, 4)
assert (p.x, p.y) == (4, 6)
def gen(n):
    for i in range(n):
        yield i * i
assert sum(gen(10)) == 285
assert 2 ** 100 == 1267650600228229401496703205376
assert abs(sum([0.1] * 10) - 1.0) < 1e-12
assert "{:>6.2f}|{}".format(3.14159, [1, "a"]) == "  3.14|[1, 'a']"
assert f"{7 * 6}" == "42"
try:
    {}["missing"]
except KeyError as e:
    assert e.args[0] == "missing"
assert sorted({3: 1, 1: 2, 2: 3}) == [1, 2, 3]
assert [c for c in "héllo"] == ["h", "é", "l", "l", "o"]

# the standard library
assert json.loads(json.dumps({"a": [1, 2.5, None, True]})) == {"a": [1, 2.5, None, True]}
assert re.match(r"(\w+)@(\w+)", "user@palladium").group(2) == "palladium"
assert struct.unpack("<hI", struct.pack("<hI", -2, 70000)) == (-2, 70000)
assert binascii.hexlify(hashlib.sha256(b"abc").digest())[:16] == b"ba7816bf8f01cfea"
random.seed(1)
assert 1 <= random.randint(1, 6) <= 6

# memory (MicroPython's heap is 2 MB unless -X heapsize= says otherwise)
big = bytearray(1024 * 1024)
big[-1] = 7
assert len(big) == 1048576 and big[-1] == 7
del big
import gc; gc.collect()
items = [str(i) for i in range(10000)]
assert items[-1] == "9999"

# files and directories
d = "/tmp/mpy-check"
os.mkdir(d)
with open(d + "/f.txt", "w") as f:
    for i in range(100):
        f.write("%d\n" % i)
with open(d + "/f.txt") as f:
    assert sum(int(line) for line in f) == 4950
assert os.stat(d + "/f.txt")[6] == 290
assert "f.txt" in os.listdir(d)
os.rename(d + "/f.txt", d + "/g.txt")
try:
    os.stat(d + "/f.txt")
    assert False
except OSError as e:
    assert e.errno == errno.ENOENT
os.remove(d + "/g.txt")
os.rmdir(d)

# processes, time, environment
assert os.system("exit 3") >> 8 == 3
assert time.time() > 1700000000
t0 = time.ticks_ms()
time.sleep_ms(200)
assert 150 <= time.ticks_diff(time.ticks_ms(), t0) < 1000
assert os.getenv("NO_SUCH_VARIABLE") is None
assert sys.argv[1:] == ["arg1", "arg 2"]

print("micropython checks passed")
