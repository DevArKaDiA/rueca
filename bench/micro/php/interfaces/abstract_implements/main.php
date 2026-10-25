<?php
interface Handler
{
    public function handle(string $m): void;
}

abstract class BaseHandler implements Handler
{
    public function process(string $m): void
    {
        $this->handle($m);
    }
}
