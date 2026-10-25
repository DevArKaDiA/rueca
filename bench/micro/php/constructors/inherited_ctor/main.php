<?php
class Base
{
    public function __construct()
    {
        $this->init();
    }

    protected function init(): void
    {
    }
}

class Child extends Base
{
}

function make(): Child
{
    return new Child();
}
