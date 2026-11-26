CREATE TABLE dbo.Lines (
    OrderID INT NOT NULL,
    Amount MONEY NOT NULL
)
GO
CREATE TABLE dbo.AuditLog (
    Note NVARCHAR(200) NULL
)
GO
CREATE FUNCTION dbo.OrderTotal (@OrderID INT)
RETURNS MONEY
AS
BEGIN
    RETURN (SELECT SUM(Amount) FROM dbo.Lines WHERE OrderID = @OrderID);
END
GO
CREATE TRIGGER trgLinesInsert ON dbo.Lines
AFTER INSERT
AS
    INSERT INTO dbo.AuditLog (Note) VALUES (N'line added');
GO
