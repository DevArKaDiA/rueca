<?php
class Model
{
    public function __construct()
    {
    }

    public static function create(): static
    {
        return new static();
    }
}

class Post extends Model
{
}

function seed(): Post
{
    return Post::create();
}
