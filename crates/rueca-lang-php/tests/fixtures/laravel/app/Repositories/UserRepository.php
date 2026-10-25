<?php

namespace App\Repositories;

use App\Models\User;

class UserRepository
{
    public function create(array $attributes): User
    {
        return User::create($attributes);
    }

    public function active(int $limit): array
    {
        return User::active($limit);
    }
}
