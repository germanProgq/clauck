"""A deliberately small calculator used by the Clauck demo."""

import sys


def add(a: int, b: int) -> int:
    return a + b


if __name__ == "__main__":
    print(add(int(sys.argv[1]), int(sys.argv[2])))
