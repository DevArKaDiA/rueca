<?php

namespace Tests\Unit;

use App\Contracts\Notifier;
use App\Repositories\UserRepository;
use App\Services\UserService;
use PHPUnit\Framework\Attributes\Test;
use Tests\TestCase;

class UserServiceTest extends TestCase
{
    private UserService $service;

    protected function setUp(): void
    {
        parent::setUp();
        $this->service = new UserService(new UserRepository(), $this->createMock(Notifier::class));
    }

    public function testRegisterRequiresEmail(): void
    {
        $this->expectException(\InvalidArgumentException::class);
        $this->service->register([]);
    }

    /** @test */
    public function it_lists_active_users(): void
    {
        $this->assertSame([], $this->service->active(5));
    }

    #[Test]
    public function registers(): void
    {
        $this->service->register(['email' => 'a@b.c']);
    }
}
