<?php
class Money
{
    private function __construct(private int $cents)
    {
    }

    public static function of(int $cents): self
    {
        return new self($cents);
    }

    public function add(Money $o): self
    {
        return new self($this->cents + $o->cents);
    }
}

function total(): Money
{
    return Money::of(1)->add(Money::of(2));
}
