# syntax=docker/dockerfile:1
ARG PYTHON_VERSION=3.12

# Build stage: install dependencies.
FROM python:${PYTHON_VERSION}-slim AS build
ENV PIP_NO_CACHE_DIR=1 \
    APP_HOME=/app
WORKDIR $APP_HOME
COPY pyproject.toml .
RUN pip install .

FROM build AS test
RUN pytest

FROM python:${PYTHON_VERSION}-slim
ARG RELEASE
ENV DATABASE_URL=postgres://db/shop
COPY --from=build /app /app
CMD ["python", "-m", "app.main"]
