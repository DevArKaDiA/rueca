<?php
class Handler
{
    public function __invoke(string $m): string
    {
        return $m;
    }
}

function dispatch(): string
{
    $h = new Handler();
    return $h('x');
}
