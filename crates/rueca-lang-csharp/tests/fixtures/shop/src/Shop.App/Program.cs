using Acme.Shop.Services;
using Store = Acme.Shop.Services.IOrderStore;

namespace Acme.Shop.App;

internal sealed class MemoryStore : Store
{
    private readonly Dictionary<int, Acme.Shop.Models.Order> _orders = new();

    public void Save(Acme.Shop.Models.Order order) => _orders[order.Id] = order;

    public Acme.Shop.Models.Order? Find(int id) => _orders.GetValueOrDefault(id);
}

internal static class Program
{
    private static void Main()
    {
        var service = new OrderService(new MemoryStore(), null!);
        service.Place(1);
        Helpers.Log("placed");
    }
}

internal static class Helpers
{
    public static void Log(string message) => Console.WriteLine(message);
}
