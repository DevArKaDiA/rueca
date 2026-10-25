<?php

namespace Acme\Blog\Entity;

class Post
{
    public function __construct(
        #[\SensitiveParameter] public string $title = '',
        protected ?\DateTimeImmutable $publishedAt = null,
    ) {
    }

    public function publish(): static
    {
        $this->publishedAt = new \DateTimeImmutable();

        return $this;
    }
}
