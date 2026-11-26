namespace Acme.Shop.Models;

public abstract class Entity
{
    protected Entity(int key) { Key = key; }

    public int Key { get; }

    public virtual void Validate() { }
}
