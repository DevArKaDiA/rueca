<?php
enum Status
{
    case Active;
    case Banned;

    public function color(): string
    {
        return $this === self::Active ? 'green' : 'red';
    }
}

function badge(): string
{
    return Status::Active->color();
}
