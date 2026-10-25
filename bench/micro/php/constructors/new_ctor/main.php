<?php
class Point
{
    public function __construct(public int $x = 0, public int $y = 0)
    {
    }
}

function origin(): Point
{
    return new Point();
}
