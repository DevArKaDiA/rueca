<?php
interface Shape
{
    public function area(): float;
}

class Square implements Shape
{
    public function area(): float
    {
        return 1.0;
    }
}

function describe(Square $s): string
{
    return (string) $s->area();
}
