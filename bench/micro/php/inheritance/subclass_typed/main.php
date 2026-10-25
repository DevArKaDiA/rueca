<?php
class Repo
{
    public function find(int $id): ?array
    {
        return null;
    }
}

class UserRepo extends Repo
{
    public function active(): array
    {
        return [];
    }
}

function load(UserRepo $r): void
{
    $r->find(1);
    $r->active();
}
