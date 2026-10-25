<?php

namespace App\Http\Controllers;

abstract class Controller
{
    protected function authorizeAccess(string $ability): void
    {
    }
}
