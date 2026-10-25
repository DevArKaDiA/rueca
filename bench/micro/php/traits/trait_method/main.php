<?php
trait Greets
{
    public function greet(): string
    {
        return 'hello';
    }
}

class Robot
{
    use Greets;

    public function boot(): string
    {
        return $this->greet();
    }
}
