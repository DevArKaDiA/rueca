<?php
class View
{
    public function render(): string
    {
        return '<div>';
    }
}

class Page extends View
{
    public function render(): string
    {
        return parent::render() . '</div>';
    }
}
