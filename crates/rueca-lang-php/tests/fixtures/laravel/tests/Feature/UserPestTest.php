<?php

use App\Models\User;

describe('users', function () {
    beforeEach(function () {
        $this->user = User::factory()->create();
    });

    it('promotes a user', function () {
        expect($this->user->promote())->toBeInstanceOf(User::class);
    });

    test('balance starts at zero', fn () => expect(User::find(1)->balance())->toBe(0));
});

it('lists users', function () {
    $this->get('/users')->assertOk();
})->skip(false);
