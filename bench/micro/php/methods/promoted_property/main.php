<?php
class Clock
{
    public function now(): int
    {
        return time();
    }
}

class Service
{
    public function __construct(private Clock $clock)
    {
    }

    public function stamp(): int
    {
        return $this->clock->now();
    }
}

function make(): Service
{
    return new Service(new Clock());
}
