<?php

namespace App\Services;

use App\Contracts\Notifier;
use App\Models\User;
use App\Repositories\UserRepository;
use InvalidArgumentException;

final class UserService
{
    public function __construct(
        private UserRepository $repository,
        private Notifier $notifier,
    ) {
    }

    public function register(array $data): User
    {
        if (empty($data['email'])) {
            throw new InvalidArgumentException('email required');
        }
        $user = $this->repository->create($data);
        $this->notifier->notify($user, 'welcome');
        $user->promote();

        return $user;
    }

    /** @return User[] */
    public function active(int $limit): array
    {
        return $this->repository->active($limit);
    }
}
