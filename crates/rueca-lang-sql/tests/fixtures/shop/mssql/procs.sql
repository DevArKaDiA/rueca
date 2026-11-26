CREATE TABLE [dbo].[audit_log] ([id] INT IDENTITY(1,1) NOT NULL, [note] NVARCHAR(200) NULL)
GO
CREATE PROCEDURE [dbo].[Audit] @note NVARCHAR(200)
AS
    INSERT INTO dbo.audit_log (note) VALUES (@note);
GO
CREATE PROCEDURE [dbo].[ReportOrders]
AS
BEGIN
    SET NOCOUNT ON;
    SELECT id FROM orders;
    EXEC dbo.Audit @note = N'report';
END
GO
