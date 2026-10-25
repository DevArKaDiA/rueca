<?php

declare(strict_types=1);

namespace App\Http\Controllers;

use App\Enums\Status;
use App\Http\Requests\StoreUserRequest;
use App\Models\User;
use App\Services\UserService;
use Illuminate\Http\JsonResponse;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\{Cache, Log};

use function App\Support\format_money;

/**
 * CRUD for users.
 */
class UserController extends Controller
{
    public const PER_PAGE = 20;

    public function __construct(private readonly UserService $users)
    {
    }

    /** List active users. */
    public function index(Request $request): JsonResponse
    {
        $this->authorizeAccess('viewAny');
        $page = (int) $request->query('page', 1);
        $users = Cache::remember("users.$page", 60, fn () => $this->users->active(self::PER_PAGE));

        return response()->json($users);
    }

    public function store(StoreUserRequest $request): JsonResponse
    {
        $user = $this->users->register($request->validated());
        Log::info('registered', ['id' => $user->id]);

        return new JsonResponse(['balance' => format_money($user->balance())], 201);
    }

    public function show(int $id): User
    {
        $user = User::findOrFail($id);
        if ($user->status === Status::Banned) {
            throw new \DomainException('banned');
        }

        return $user;
    }
}
