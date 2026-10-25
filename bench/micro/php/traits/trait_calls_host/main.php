<?php
trait Validates
{
    public function validate(): bool
    {
        return $this->rules() !== [];
    }
}

class Form
{
    use Validates;

    public function rules(): array
    {
        return ['name' => 'required'];
    }
}
