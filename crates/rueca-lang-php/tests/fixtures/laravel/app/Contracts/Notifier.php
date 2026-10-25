<?php

namespace App\Contracts;

use App\Models\User;

interface Notifier
{
    public const CHANNEL = 'mail';

    public function notify(User $user, string $message): bool;
}
