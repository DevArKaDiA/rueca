import { Body, Controller, Get, Param, Post } from '@nestjs/common';
import { UserService } from './users.service';
import { CreateUserDto } from './dto';

@Controller('users')
export class UsersController {
  constructor(private readonly users: UserService) {}

  @Get(':id')
  findOne(@Param('id') id: string) {
    return this.users.findById(id);
  }

  @Post()
  create(@Body() dto: CreateUserDto) {
    return this.users.create(dto);
  }
}
