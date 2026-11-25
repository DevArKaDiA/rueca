namespace App;

public class Cat
{
    public string Speak()
    {
        return "meow";
    }
}

public class Dog
{
    public string Speak()
    {
        return "woof";
    }
}

public class Owner
{
    public string Call(Dog dog)
    {
        return dog.Speak();
    }
}
