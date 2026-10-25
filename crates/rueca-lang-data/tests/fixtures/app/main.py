import os
from os import environ, getenv


def database_url():
    return os.getenv("DATABASE_URL", "sqlite://")


def main():
    level = os.environ["LOG_LEVEL"]
    token = os.environ.get("API_TOKEN")
    queue = environ.get("QUEUE", "default")
    dsn = getenv("SENTRY_DSN")
    name = "DYNAMIC"
    ignored = os.getenv(name)
    return level, token, queue, dsn, ignored, database_url()
