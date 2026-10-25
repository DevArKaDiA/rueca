<?php
class Address
{
    public function city(): string
    {
        return 'x';
    }
}

class User
{
    public function address(): ?Address
    {
        return null;
    }
}

function cityOf(?User $u): ?string
{
    return $u?->address()?->city();
}
