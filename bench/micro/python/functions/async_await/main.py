import asyncio


async def fetch(url):
    await asyncio.sleep(0)
    return url.upper()


async def parse(body):
    await asyncio.sleep(0)
    return body.split()


def summarize(words):
    count = len(words)
    return count


async def crawl(url):
    body = await fetch(url)
    words = await parse(body)
    return summarize(words)


def main():
    result = asyncio.run(crawl("x"))
    return result
