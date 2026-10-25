<?php
interface Shape
{
    public function area(): float;
}

class Circle implements Shape
{
    public function area(): float
    {
        return 3.14;
    }
}

function describe(Shape $s): string
{
    return (string) $s->area();
}
