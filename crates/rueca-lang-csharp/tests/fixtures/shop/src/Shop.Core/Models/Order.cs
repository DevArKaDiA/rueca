namespace Acme.Shop.Models;

/// <summary>An order.</summary>
public partial class Order : Entity
{
    public int Id { get; init; }
    public List<Line> Lines { get; } = new();

    public Order(int id) : base(id)
    {
        Id = id;
    }

    public void Add(Line line) => Lines.Add(line);

    public void Add(string sku, int quantity = 1) => Add(new Line(sku, quantity));
}

public record Line(string Sku, int Quantity);
