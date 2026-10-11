import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from src.calculator import add

assert add(2, 3) == 5
assert add(-1, 1) == 0
print("ok")
