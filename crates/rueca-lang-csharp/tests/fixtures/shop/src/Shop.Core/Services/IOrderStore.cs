namespace Acme.Shop.Services;

public interface IOrderStore
{
    void Save(Order order);
    Order? Find(int id);
}
