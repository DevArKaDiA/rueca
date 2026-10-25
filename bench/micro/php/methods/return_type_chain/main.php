<?php
class Engine
{
    public function start(): void
    {
    }
}

class Car
{
    public function engine(): Engine
    {
        return new Engine();
    }

    public function drive(): void
    {
        $this->engine()->start();
    }
}
