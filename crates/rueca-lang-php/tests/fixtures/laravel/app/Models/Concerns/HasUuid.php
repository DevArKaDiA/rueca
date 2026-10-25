<?php

namespace App\Models\Concerns;

use Illuminate\Support\Str;

trait HasUuid
{
    public function ensureUuid(): string
    {
        return $this->uuid ??= (string) Str::uuid();
    }
}
