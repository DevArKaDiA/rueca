<?php

namespace App\Models;

use App\Enums\Status;
use App\Models\Concerns\HasUuid;
use Illuminate\Database\Eloquent\Factories\HasFactory;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;

/**
 * An account holder.
 *
 * @property int $id
 */
class User extends Model
{
    use HasFactory, HasUuid;

    public const TABLE = 'users';

    protected $fillable = ['name', 'email'];

    protected $casts = ['status' => Status::class];

    public function orders(): HasMany
    {
        return $this->hasMany(Order::class);
    }

    public function balance(): int
    {
        return $this->orders()->sum('total') - $this->refunds;
    }

    public static function active(int $limit): array
    {
        return static::query()->where('status', Status::Active)->limit($limit)->get()->all();
    }

    public function promote(): static
    {
        $this->status = Status::Active;
        $this->ensureUuid();

        return $this;
    }
}
