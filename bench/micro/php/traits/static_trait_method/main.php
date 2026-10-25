<?php
trait Factory
{
    public static function make(): static
    {
        return new static();
    }
}

class Widget
{
    use Factory;
}

function widget(): Widget
{
    return Widget::make();
}
