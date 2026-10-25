from functools import wraps


def backoff(attempt):
    delay = 2**attempt
    return delay


def retry(times):
    def deco(func):
        @wraps(func)
        def wrapper(*args, **kwargs):
            for attempt in range(times):
                try:
                    return func(*args, **kwargs)
                except IOError:
                    backoff(attempt)
            return None

        return wrapper

    return deco


def download(url):
    data = url.encode()
    return data


@retry(3)
def fetch(url):
    body = download(url)
    return body


def main():
    page = fetch("http://x")
    return page
