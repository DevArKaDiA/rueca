<?php
class Base
{
    public function __construct(protected string $name)
    {
    }
}

class Child extends Base
{
    public function __construct()
    {
        parent::__construct('child');
    }
}

function make(): Child
{
    return new Child();
}
