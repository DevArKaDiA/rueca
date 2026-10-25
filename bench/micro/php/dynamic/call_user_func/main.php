<?php
function helper(): void
{
}

class Task
{
    public function run(): void
    {
        call_user_func([$this, 'step']);
        call_user_func('helper');
    }

    public function step(): void
    {
    }
}
