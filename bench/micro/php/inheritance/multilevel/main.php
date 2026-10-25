<?php
class A
{
    public function fromA(): void
    {
    }
}

class B extends A
{
}

class C extends B
{
    public function own(): void
    {
        $this->fromA();
    }
}

function use_c(C $c): void
{
    $c->fromA();
}
