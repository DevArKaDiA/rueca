namespace App;

public class Entity
{
    public Entity(int id)
    {
        Validate();
    }

    public void Validate()
    {
    }
}

public class Customer : Entity
{
    public Customer(int id) : base(id)
    {
    }
}
