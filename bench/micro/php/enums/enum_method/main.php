<?php
enum Suit: string
{
    case Hearts = 'H';
    case Spades = 'S';

    public function label(): string
    {
        return self::prefix() . $this->value;
    }

    public static function prefix(): string
    {
        return 'suit:';
    }
}

function show(Suit $s): string
{
    return $s->label();
}
