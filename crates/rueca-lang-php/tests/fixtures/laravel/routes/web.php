<?php

use App\Http\Controllers\UserController;
use Illuminate\Support\Facades\Route;

require __DIR__ . '/../app/Support/helpers.php';

Route::get('/users', [UserController::class, 'index'])->name('users.index');
Route::post('/users', [UserController::class, 'store']);
