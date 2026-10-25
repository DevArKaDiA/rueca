from typing import Protocol


class Closer(Protocol):
    def close(self) -> None: ...


class File:
    def close(self) -> None:
        self.closed = True


class Socket:
    def close(self) -> None:
        self.closed = True


def shutdown(resource: Closer):
    resource.close()
    return True


def main():
    f = File()
    shutdown(f)
    f.close()
